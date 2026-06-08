# Serverless — RedDB Domain Glossary

Part of the [glossary map](../CONTEXT-MAP.md). The fast-boot, read-heavy, object-storage-distributed single-node posture. The shared storage engine underneath lives in [Persistence](persistence.md); the local-disk embedded posture lives in [Standalone](standalone.md).

## Serverless profile

- **Serverless storage profile** — RedDB posture optimized for fast boot, read-heavy access, hot copy, and multipart/snapshot distribution rather than long-lived local disk assumptions. Its canonical database artifact remains an `.rdb`, but serverless runtimes may export or hydrate a derived segment pack for object storage, hot boot, and incremental copy.
