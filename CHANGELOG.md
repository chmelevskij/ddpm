# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/chmelevskij/ddpm/compare/v0.1.0...v0.2.0) - 2026-10-18

### Added

- radio-button input list with custom names and per-input shortcuts
- keyboard shortcuts to cycle monitor inputs
- macOS support
- add system tray icon with scroll brightness and input switching

### Fixed

- crash when renaming an input

### Other

- make `just install` work on macOS
- install libudev-dev for release-plz cargo package
- fix release-plz version detection for unpublished crate
- add MIT license
- release v0.1.0 ([#1](https://github.com/chmelevskij/ddpm/pull/1))
- suffix f32 literals in Stroke::new for newer clippy

## [0.1.0](https://github.com/chmelevskij/ddpm/releases/tag/v0.1.0) - 2026-08-31

### Other

- suffix f32 literals in Stroke::new for newer clippy
- add repository metadata to Cargo.toml
- add CI and release-plz release workflows
- add README with screenshots
- Rework UI around a DDC worker thread; robustness and theming
- Add justfile and desktop entry
- Initial commit: egui DDC/CI monitor control
