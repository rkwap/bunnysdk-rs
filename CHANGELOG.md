# Changelog

## 0.1.0

First release.

- Clients for Core, Compute (Edge Scripting), Shield, Stream (including live
  streams), Magic Containers, Database, Origin Errors and Partner, generated
  from bunny.net's official OpenAPI specs.
- Hand-written clients for Edge Storage (streamed uploads and downloads,
  SHA-256 checksums) and Logging (v1 and v2).
- Stream resumable (TUS) uploads and presigned upload credentials.
- One `Error` type normalising bunny.net's error formats into `ApiError`.
- `Config`: idle timeout, opt-in retries, custom `reqwest::Client`.
- One Cargo feature per API.
