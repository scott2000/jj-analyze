# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Compatible with `jj` 0.37.0.

### Added

* New `-O`/`--no-optimize` flag to disable revset optimizations.
* Extra `-r` argument is now allowed for consistency with `jj`.

### Changed

* Updated colors to make output easier to read.
* Updated field order to match `jj` revset engine internals.
* Renamed operations to better match `jj` revset engine internals.

## [0.1.0] - 2026-01-12

Initial release of `jj-analyze`. Compatible with `jj` 0.37.0.

[Unreleased]: https://github.com/scott2000/jj-analyze/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/scott2000/jj-analyze/releases/tag/v0.1.0
