# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.0](https://github.com/ynishi/persona-wire/compare/v0.13.0...v0.14.0) - 2026-07-11

### Added

- *(core)* unified adapter filter IF — FilterCap vocabulary + WireFilters parser (Phase 1)

## [0.13.0](https://github.com/ynishi/persona-wire/compare/v0.12.1...v0.13.0) - 2026-07-11

### Added

- add indirect auth reference layer for adapters (AuthSpec / Bearer)
- *(adapters)* add matrix:// and mastodon:// (Phase 1, Bearer)

### Other

- *(onboarding)* list mcp / sqlite / apple-notes / persona-pack / activitypub / bluesky adapters

## [0.12.1](https://github.com/ynishi/persona-wire/compare/v0.12.0...v0.12.1) - 2026-07-09

### Added

- *(adapters)* add Wave 2 (apple-notes / activitypub / bluesky)

### Other

- *(aidoc)* integrate cargo-aidoc + commit LLM-facing artifacts

## [0.12.0](https://github.com/ynishi/persona-wire/compare/v0.11.0...v0.12.0) - 2026-07-08

### Added

- *(adapter-github)* implement Pageable with Link header cursor path
- *(core)* wire-layer pagination driver and Pageable capability check
- *(core)* add Pageable trait + Cursor enum for adapter pagination
- *(adapter-todoist)* implement Pageable with NextToken cursor threading
- *(adapter-notion)* implement Pageable for all 4 kinds via NextToken
- *(adapter-slack)* implement Pageable for channels + history via NextToken

### Other

- *(adapters)* [**breaking**] internalize pagination in Adapter::fetch, drop Pageable/Cursor
- *(adapters)* drop parse-time limit>MAX_LIMIT gate on todoist/notion/slack
