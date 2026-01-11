# jj-analyze

`jj-analyze` is a tool for [jj](https://www.jj-vcs.dev/latest/) that can
analyze a revset and display a tree showing how the revset will be evaluated.
It can also help debug performance problems by pointing out potentially
expensive operations.

## Installation

`jj-analyze` can be installed using `cargo install`:

```shell
$ cargo install --locked jj-analyze
```

## Compatibility

The current version of `jj-analyze` is compiled to use `jj` version 0.37.0, but
it should be backwards compatible with other recent versions.

## Example

*Note: These examples may be easier to read when run locally since
`jj-analyze` supports colored output.*

We can look at the default log revset as an example. For simplicity, I have
omitted `present()` since it does not affect the output.

```shell
$ jj-analyze '@ | ancestors(immutable_heads().., 2) | trunk()'
Union [
  @
  Ancestors {
    generation: 0..2
    heads: Range {
      roots: builtin_immutable_heads()
      heads: visible_heads() and referenced revisions
    }
  }
  trunk()
]
```

The built-in revsets `trunk()` and `builtin_immutable_heads()` are collapsed by
default, but they can be expanded using `-B`/`--no-collapse-builtin`.

## Debugging performance problems

`jj-analyze` can also be used to debug performance problems. When running in a
terminal that supports colors, each operation is colored based on how it was
evaluated. Evaluation is categorized into 3 types:

* **Eager** (blue): Matching revisions are immediately fully collected into a list.
* **Lazy** (cyan): Matching revisions are streamed lazily, and evaluation can
  be terminated early if no longer required.
* **Predicate** (magenta): Instead of directly producing revisions, the revset
  is treated as a boolean predicate on revisions.

### The problem

Often, revset performance problems are caused due to eager evaluation of large
revsets. For instance, the revset `latest(empty())` can be slow in large repos.
We can analyze it to see the problem:

```shell
$ jj-analyze 'latest(empty())'
Latest {
  count: 1
  candidates: Filter {
    predicate: empty()
    candidates: (EXPENSIVE) Ancestors {
      heads: visible_heads()
    }
  }
}
```

Since `Latest` requires its `candidates` to be evaluated eagerly, `jj` must scan
all ancestors of `visible_heads()` and evaluate the predicate `empty()` on every
revision in the entire history of the repo. Therefore, the `(EXPENSIVE)` label
indicates that this `Ancestors` operation may be expensive. `jj-analyze` adds an
`(EXPENSIVE)` label like this whenever it sees eager evaluation of a revset
which is likely to produce a large number of revisions.

### Solution 1

For cases like this, a common fix is to intersect the predicate with another
revset to restrict the range of commits that needs to be searched. Usually
`mutable()` is a good choice:

```shell
$ jj-analyze 'latest(empty() & mutable())'
Latest {
  count: 1
  candidates: Filter {
    predicate: empty()
    candidates: Range {
      roots: Union [
        builtin_immutable_heads()
        root()
      ]
      heads: visible_heads() and referenced revisions
    }
  }
}
```

The `Ancestors` operation has been replaced with a `Range` operation, which
means that `jj` no longer needs to evaluate the `empty()` predicate on the
entire history of the repo. Instead, it can stop scanning early whenever it
reaches an immutable revision, since now we've told it that we don't care about
any revisions which are ancestors of immutable revisions.

### Solution 2

Another common solution for performance problems is to use `heads()` when we
only care about the most recent commits in a revset. For instance, `latest()`
normally compares the committer timestamps of every revision in `candidates`.
However, we know that a revision's parents usually should have an earlier
committer timestamp than the revision itself, so it's generally sufficient to
only compare the "heads", since these will be the most recently added commits.

Therefore, we can add `heads()` to our revset get an even more efficient version:

```shell
$ jj-analyze 'latest(heads(empty() & mutable()))'
Latest {
  count: 1
  candidates: HeadsRange {
    filter: empty()
    roots: Union [
      builtin_immutable_heads()
      root()
    ]
    heads: visible_heads() and referenced revisions
  }
}
```

Since we added `heads()`, the revset optimizer was able to combine the `Heads`
operation with the `Filter` and `Range` operations to produce a `HeadsRange`
operation. Similarly to before, `jj` can still stop scanning when it reaches an
immutable revision, but now it can also stop scanning early when it finds a
revision that matches the `empty()` predicate.

## Usage

```shell
$ jj-analyze --help
Analyze a revset and display a tree showing how it will be evaluated

Potentially expensive operations are indicated with an `(EXPENSIVE)` label. When
color is enabled, operations are also colored based on how they are evaluated.
Eager evaluation is indicated by blue, lazy evaluation is indicated by cyan, and
predicates are indicated by magenta.

This tool attempts to match the default index implementation's revset engine as
well as possible. If you use a custom build of `jj` which uses a different index
implementation, analysis results may not be accurate.

To make the output easier to read, nested union, intersection, and coalesce
operations are flattened, and some operations have been renamed for clarity.

Usage: jj-analyze [OPTIONS] <REVSET>

Arguments:
  <REVSET>
          A revset to analyze

Options:
      --collapse <ALIAS>
          Collapses the provided revset alias, hiding it from the output

      --color <MODE>
          When to colorize output

          [possible values: auto, never, always]

  -c, --context <CONTEXT>
          Base context for evaluation of revset

          For instance, if the entire revset will be iterated over, using
          `--context eager` may give more accurate analysis results. By default,
          lazy evaluation of the base revset is assumed.

          [default: lazy]
          [possible values: eager, lazy, predicate]

  -d, --define <DEFINE>
          Define a custom revset alias

          For example, `--define 'immutable_heads()=none()' will override
          `immutable_heads()` to be `none()`.

  -A, --no-analyze
          Disable analysis of evaluation and cost

          If you are using a different revset backend, the analysis features may
          not be useful, so this flag disables them. When using this option, all
          unresolved nodes are printed in blue.

  -B, --no-collapse-builtin
          Disable collapsing of builtin revset aliases

          By default, `trunk()` and `builtin_immutable_heads()` are collapsed to
          make the output easier to read.

  -C, --no-config
          Disable loading user-defined revset aliases

  -R, --repository <PATH>
          Path to repository to load revset aliases from

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```
