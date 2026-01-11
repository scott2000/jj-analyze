use std::collections::BTreeMap;
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::stream::BoxStream;
use indexmap::IndexSet;
use jj_lib::backend::Backend;
use jj_lib::backend::BackendResult;
use jj_lib::backend::ChangeId;
use jj_lib::backend::Commit;
use jj_lib::backend::CommitId;
use jj_lib::backend::CopyHistory;
use jj_lib::backend::CopyId;
use jj_lib::backend::CopyRecord;
use jj_lib::backend::FileId;
use jj_lib::backend::SigningFn;
use jj_lib::backend::SymlinkId;
use jj_lib::backend::Tree;
use jj_lib::backend::TreeId;
use jj_lib::index::Index;
use jj_lib::index::IndexResult;
use jj_lib::index::ResolvedChangeTargets;
use jj_lib::object_id::HexPrefix;
use jj_lib::object_id::ObjectId;
use jj_lib::object_id::PrefixResolution;
use jj_lib::op_store::OpStore;
use jj_lib::op_store::RefTarget;
use jj_lib::op_store::RemoteRefState;
use jj_lib::ref_name::WorkspaceName;
use jj_lib::repo::ReadonlyRepo;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPath;
use jj_lib::repo_path::RepoPathBuf;
use jj_lib::revset::ResolvedRevsetExpression;
use jj_lib::revset::RevsetCommitRef;
use jj_lib::revset::RevsetDiagnostics;
use jj_lib::revset::RevsetExpression;
use jj_lib::revset::RevsetParseContext;
use jj_lib::revset::UserRevsetExpression;
use jj_lib::revset::{self};
use jj_lib::signing::Signer;
use jj_lib::store::Store;
use jj_lib::str_util::StringExpression;
use jj_lib::str_util::StringPattern;
use jj_lib::submodule_store::SubmoduleStore;
use jj_lib::tree_merge::MergeOptions;
use jj_lib::view::View;
use tokio::io::AsyncRead;

use crate::expr::Expr;
use crate::expr::ResolvedReference;
use crate::print::format_string_expression;

pub fn parse<'a>(
    input: &str,
    context: &RevsetParseContext,
    reference_map: &'a mut ReferenceMap,
) -> anyhow::Result<Expr<'a>> {
    let dummy_backend: Box<dyn Backend> = Box::new(DummyBackend {
        root_commit_id: reference_map.insert(ResolvedReference::root()),
    });
    let mut visible_heads = HashSet::new();
    visible_heads.insert(reference_map.insert(ResolvedReference::visible_heads()));
    let dummy_repo = DummyRepo {
        store: Store::new(
            dummy_backend,
            Signer::new(None, vec![]),
            MergeOptions {
                hunk_level: jj_lib::files::FileMergeHunkLevel::Line,
                same_change: jj_lib::merge::SameChange::Accept,
            },
        ),
        view: View::new(jj_lib::op_store::View {
            head_ids: visible_heads,
            local_bookmarks: BTreeMap::new(),
            local_tags: BTreeMap::new(),
            remote_views: BTreeMap::new(),
            git_refs: BTreeMap::new(),
            git_head: RefTarget::absent(),
            wc_commit_ids: BTreeMap::new(),
        }),
    };

    let mut diagnostics = RevsetDiagnostics::new();
    let parsed =
        revset::parse(&mut diagnostics, input, context).context("Failed to parse revset")?;
    let resolved = resolve_user_expressions(&parsed, None, reference_map);
    let optimized = revset::optimize(resolved);
    let backend = optimized.to_backend_expression(&dummy_repo);
    Ok(Expr::parse(backend, reference_map))
}

fn resolve_user_expressions(
    expr: &UserRevsetExpression,
    operation: Option<&str>,
    reference_map: &mut ReferenceMap,
) -> Arc<ResolvedRevsetExpression> {
    let mapped = match expr {
        RevsetExpression::None => RevsetExpression::None,
        RevsetExpression::All => RevsetExpression::All,
        RevsetExpression::VisibleHeads => RevsetExpression::VisibleHeads,
        RevsetExpression::VisibleHeadsOrReferenced => RevsetExpression::VisibleHeadsOrReferenced,
        RevsetExpression::Root => RevsetExpression::Root,
        RevsetExpression::Commits(commit_ids) => RevsetExpression::Commits(commit_ids.clone()),
        RevsetExpression::CommitRef(reference) => {
            let resolved = match reference {
                RevsetCommitRef::WorkingCopy(workspace) if workspace == WorkspaceName::DEFAULT => {
                    ResolvedReference::working_copy()
                }
                RevsetCommitRef::WorkingCopy(workspace) => {
                    ResolvedReference::new_owned(format!("{}@", workspace.as_str()))
                }
                RevsetCommitRef::WorkingCopies => ResolvedReference::new_static("working_copies()"),
                RevsetCommitRef::Symbol(symbol) => ResolvedReference::new_owned(symbol.clone()),
                RevsetCommitRef::RemoteSymbol(symbol) => {
                    ResolvedReference::new_owned(symbol.to_string())
                }
                RevsetCommitRef::ChangeId(hex_prefix) => {
                    ResolvedReference::new_owned(format!("change_id({})", hex_prefix.reverse_hex()))
                }
                RevsetCommitRef::CommitId(hex_prefix) => {
                    ResolvedReference::new_owned(format!("commit_id({})", hex_prefix.hex()))
                }
                RevsetCommitRef::Bookmarks(StringExpression::Pattern(p)) if is_all_pattern(p) => {
                    ResolvedReference::new_static("bookmarks()")
                }
                RevsetCommitRef::Bookmarks(bookmark) => ResolvedReference::new_owned(format!(
                    "bookmarks({})",
                    format_string_expression(bookmark)
                )),
                RevsetCommitRef::RemoteBookmarks {
                    bookmark: StringExpression::Pattern(b),
                    remote: StringExpression::Pattern(r),
                    remote_ref_state,
                } if is_all_pattern(b) && is_all_pattern(r) => match remote_ref_state {
                    None => ResolvedReference::new_static("remote_bookmarks()"),
                    Some(RemoteRefState::New) => {
                        ResolvedReference::new_static("untracked_remote_bookmarks()")
                    }
                    Some(RemoteRefState::Tracked) => {
                        ResolvedReference::new_static("tracked_remote_bookmarks()")
                    }
                },
                RevsetCommitRef::RemoteBookmarks {
                    bookmark,
                    remote,
                    remote_ref_state,
                } => match remote_ref_state {
                    None => ResolvedReference::new_owned(format!(
                        "remote_bookmarks({}, remote={})",
                        format_string_expression(bookmark),
                        format_string_expression(remote)
                    )),
                    Some(RemoteRefState::New) => ResolvedReference::new_owned(format!(
                        "untracked_remote_bookmarks({}, remote={})",
                        format_string_expression(bookmark),
                        format_string_expression(remote)
                    )),
                    Some(RemoteRefState::Tracked) => ResolvedReference::new_owned(format!(
                        "tracked_remote_bookmarks({}, remote={})",
                        format_string_expression(bookmark),
                        format_string_expression(remote)
                    )),
                },
                RevsetCommitRef::Tags(StringExpression::Pattern(p)) if is_all_pattern(p) => {
                    ResolvedReference::new_static("tags()")
                }
                RevsetCommitRef::Tags(tag) => {
                    ResolvedReference::new_owned(format!("tags({})", format_string_expression(tag)))
                }
                RevsetCommitRef::GitRefs => ResolvedReference::new_static("git_refs()"),
                RevsetCommitRef::GitHead => ResolvedReference::new_static("git_head()"),
            };
            if let Some(operation) = operation {
                let with_operation =
                    ResolvedReference::new_owned(format!("{resolved} at operation {operation}"));
                RevsetExpression::Commits(vec![reference_map.insert(with_operation)])
            } else {
                RevsetExpression::Commits(vec![reference_map.insert(resolved)])
            }
        }
        RevsetExpression::Ancestors {
            heads,
            generation,
            parents_range,
        } => {
            let heads = resolve_user_expressions(heads, operation, reference_map);
            let generation = generation.clone();
            let parents_range = parents_range.clone();
            RevsetExpression::Ancestors {
                heads,
                generation,
                parents_range,
            }
        }
        RevsetExpression::Descendants { roots, generation } => {
            let roots = resolve_user_expressions(roots, operation, reference_map);
            let generation = generation.clone();
            RevsetExpression::Descendants { roots, generation }
        }
        RevsetExpression::Range {
            roots,
            heads,
            generation,
            parents_range,
        } => {
            let roots = resolve_user_expressions(roots, operation, reference_map);
            let heads = resolve_user_expressions(heads, operation, reference_map);
            let generation = generation.clone();
            let parents_range = parents_range.clone();
            RevsetExpression::Range {
                roots,
                heads,
                generation,
                parents_range,
            }
        }
        RevsetExpression::DagRange { roots, heads } => {
            let roots = resolve_user_expressions(roots, operation, reference_map);
            let heads = resolve_user_expressions(heads, operation, reference_map);
            RevsetExpression::DagRange { roots, heads }
        }
        RevsetExpression::Reachable { sources, domain } => {
            let sources = resolve_user_expressions(sources, operation, reference_map);
            let domain = resolve_user_expressions(domain, operation, reference_map);
            RevsetExpression::Reachable { sources, domain }
        }
        RevsetExpression::Heads(heads) => {
            let heads = resolve_user_expressions(heads, operation, reference_map);
            RevsetExpression::Heads(heads)
        }
        RevsetExpression::HeadsRange {
            roots,
            heads,
            parents_range,
            filter,
        } => {
            let roots = resolve_user_expressions(roots, operation, reference_map);
            let heads = resolve_user_expressions(heads, operation, reference_map);
            let parents_range = parents_range.clone();
            let filter = resolve_user_expressions(filter, operation, reference_map);
            RevsetExpression::HeadsRange {
                roots,
                heads,
                parents_range,
                filter,
            }
        }
        RevsetExpression::Roots(roots) => {
            let roots = resolve_user_expressions(roots, operation, reference_map);
            RevsetExpression::Roots(roots)
        }
        RevsetExpression::ForkPoint(expression) => {
            let expression = resolve_user_expressions(expression, operation, reference_map);
            RevsetExpression::ForkPoint(expression)
        }
        RevsetExpression::Bisect(expression) => {
            let expression = resolve_user_expressions(expression, operation, reference_map);
            RevsetExpression::Bisect(expression)
        }
        RevsetExpression::HasSize { candidates, count } => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            RevsetExpression::HasSize {
                candidates,
                count: *count,
            }
        }
        RevsetExpression::Latest { candidates, count } => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            let count = *count;
            RevsetExpression::Latest { candidates, count }
        }
        RevsetExpression::Filter(predicate) => RevsetExpression::Filter(predicate.clone()),
        RevsetExpression::AsFilter(candidates) => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            RevsetExpression::AsFilter(candidates)
        }
        RevsetExpression::AtOperation {
            candidates,
            operation,
        } => {
            let candidates = resolve_user_expressions(candidates, Some(operation), reference_map);
            let visible_heads = vec![reference_map.insert(ResolvedReference(
                format!("visible_heads() at operation {operation}").into(),
            ))];
            RevsetExpression::WithinVisibility {
                candidates,
                visible_heads,
            }
        }
        RevsetExpression::WithinReference {
            candidates,
            commits,
        } => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            let commits = commits.clone();
            RevsetExpression::WithinReference {
                candidates,
                commits,
            }
        }
        RevsetExpression::WithinVisibility {
            candidates,
            visible_heads,
        } => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            let visible_heads = visible_heads.clone();
            RevsetExpression::WithinVisibility {
                candidates,
                visible_heads,
            }
        }
        RevsetExpression::Coalesce(expression1, expression2) => {
            let expression1 = resolve_user_expressions(expression1, operation, reference_map);
            let expression2 = resolve_user_expressions(expression2, operation, reference_map);
            RevsetExpression::Coalesce(expression1, expression2)
        }
        RevsetExpression::Present(candidates) => {
            let candidates = resolve_user_expressions(candidates, operation, reference_map);
            RevsetExpression::Present(candidates)
        }
        RevsetExpression::NotIn(complement) => {
            let complement = resolve_user_expressions(complement, operation, reference_map);
            RevsetExpression::NotIn(complement)
        }
        RevsetExpression::Union(expression1, expression2) => {
            let expression1 = resolve_user_expressions(expression1, operation, reference_map);
            let expression2 = resolve_user_expressions(expression2, operation, reference_map);
            RevsetExpression::Union(expression1, expression2)
        }
        RevsetExpression::Intersection(expression1, expression2) => {
            let expression1 = resolve_user_expressions(expression1, operation, reference_map);
            let expression2 = resolve_user_expressions(expression2, operation, reference_map);
            RevsetExpression::Intersection(expression1, expression2)
        }
        RevsetExpression::Difference(expression1, expression2) => {
            let expression1 = resolve_user_expressions(expression1, operation, reference_map);
            let expression2 = resolve_user_expressions(expression2, operation, reference_map);
            RevsetExpression::Difference(expression1, expression2)
        }
    };
    Arc::new(mapped)
}

fn is_all_pattern(pattern: &StringPattern) -> bool {
    matches!(pattern, StringPattern::Substring(s) if s.is_empty())
}

#[derive(Debug)]
struct DummyBackend {
    root_commit_id: CommitId,
}

#[async_trait]
impl Backend for DummyBackend {
    fn name(&self) -> &str {
        "DummyBackend"
    }

    fn commit_id_length(&self) -> usize {
        (usize::BITS / 8) as usize
    }

    fn change_id_length(&self) -> usize {
        (usize::BITS / 8) as usize
    }

    fn root_commit_id(&self) -> &CommitId {
        &self.root_commit_id
    }

    fn root_change_id(&self) -> &ChangeId {
        unimplemented!()
    }

    fn empty_tree_id(&self) -> &jj_lib::backend::TreeId {
        unimplemented!()
    }

    fn concurrency(&self) -> usize {
        1
    }

    async fn read_file(
        &self,
        _path: &RepoPath,
        _id: &FileId,
    ) -> BackendResult<Pin<Box<dyn AsyncRead + Send>>> {
        unimplemented!()
    }

    async fn write_file(
        &self,
        _path: &RepoPath,
        _contents: &mut (dyn AsyncRead + Send + Unpin),
    ) -> BackendResult<FileId> {
        unimplemented!()
    }

    async fn read_symlink(&self, _path: &RepoPath, _id: &SymlinkId) -> BackendResult<String> {
        unimplemented!()
    }

    async fn write_symlink(&self, _path: &RepoPath, _target: &str) -> BackendResult<SymlinkId> {
        unimplemented!()
    }

    async fn read_copy(&self, _id: &CopyId) -> BackendResult<CopyHistory> {
        unimplemented!()
    }

    async fn write_copy(&self, _contents: &CopyHistory) -> BackendResult<CopyId> {
        unimplemented!()
    }

    async fn get_related_copies(&self, _copy_id: &CopyId) -> BackendResult<Vec<CopyHistory>> {
        unimplemented!()
    }

    async fn read_tree(&self, _path: &RepoPath, _id: &TreeId) -> BackendResult<Tree> {
        unimplemented!()
    }

    async fn write_tree(&self, _path: &RepoPath, _contents: &Tree) -> BackendResult<TreeId> {
        unimplemented!()
    }

    async fn read_commit(&self, _id: &CommitId) -> BackendResult<Commit> {
        unimplemented!()
    }

    async fn write_commit(
        &self,
        _contents: Commit,
        _sign_with: Option<&mut SigningFn>,
    ) -> BackendResult<(CommitId, Commit)> {
        unimplemented!()
    }

    fn get_copy_records(
        &self,
        _paths: Option<&[RepoPathBuf]>,
        _root: &CommitId,
        _head: &CommitId,
    ) -> BackendResult<BoxStream<'_, BackendResult<CopyRecord>>> {
        unimplemented!()
    }

    fn gc(&self, _index: &dyn Index, _keep_newer: std::time::SystemTime) -> BackendResult<()> {
        unimplemented!()
    }
}

#[derive(Debug)]
struct DummyRepo {
    view: View,
    store: Arc<Store>,
}

impl Repo for DummyRepo {
    fn base_repo(&self) -> &ReadonlyRepo {
        unimplemented!()
    }

    fn store(&self) -> &Arc<Store> {
        &self.store
    }

    fn op_store(&self) -> &Arc<dyn OpStore> {
        unimplemented!()
    }

    fn index(&self) -> &dyn Index {
        unimplemented!()
    }

    fn view(&self) -> &View {
        &self.view
    }

    fn submodule_store(&self) -> &Arc<dyn SubmoduleStore> {
        unimplemented!()
    }

    fn resolve_change_id_prefix(
        &self,
        _prefix: &HexPrefix,
    ) -> IndexResult<PrefixResolution<ResolvedChangeTargets>> {
        unimplemented!()
    }

    fn shortest_unique_change_id_prefix_len(
        &self,
        _target_id_bytes: &ChangeId,
    ) -> IndexResult<usize> {
        unimplemented!()
    }
}

#[derive(Debug)]
pub struct ReferenceMap {
    references: IndexSet<ResolvedReference<'static>>,
}

impl ReferenceMap {
    pub fn new() -> Self {
        Self {
            references: IndexSet::new(),
        }
    }

    pub fn insert(&mut self, reference: ResolvedReference<'static>) -> CommitId {
        let index = if let Some(index) = self.references.get_index_of(&reference) {
            index
        } else {
            self.references.insert_full(reference).0
        };
        CommitId::from_bytes(&index.to_le_bytes())
    }

    pub fn get(&self, commit_id: &CommitId) -> ResolvedReference<'_> {
        let index = usize::from_le_bytes(
            commit_id
                .as_bytes()
                .try_into()
                .expect("should have correct number of bytes"),
        );
        let reference = self
            .references
            .get_index(index)
            .expect("commit ID should be present");
        ResolvedReference(reference.0.as_ref().into())
    }
}
