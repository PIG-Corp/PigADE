# ADR0004 — One durable layout history

Date:2026-10-17. Status: accepted foundation contract; UI integration pending.

The pure layout model owns placement and100-step undo/redo across tabs. Catalog owns project/workspace/tab membership. A cross-workspace transfer must remain one transaction and cannot be represented by independent per-tab undo stacks.

Store one schema-versioned LayoutHistory document plus current per-tab projections in the existing sole SQLite writer. Every edit loads and validates the document, applies to a candidate, checks membership/projections and exact JSON roundtrip, then commits document, changed tabs and selection through CAS revisions in one transaction. Publish only the committed result. Unknown/malformed state remains intact and yields an explicit error. Metadata tab creation rebases that empty tab through history without consuming placement undo; catalog membership cannot disappear through undo. Close/undo affects placement only and never restarts processes.

A bounded2MiB history and4MiB catalog envelope prevents unbounded IPC growth. Model IDs/depth/node/history limits still apply; normal serde recursion protection remains enabled. A structurally valid but JSON-too-deep candidate is rejected before persistence. The existing serde_json float_roundtrip feature preserves exact fractional split weights; default float parsing reproduced a one-ULP persistence mismatch in a new regression. Both application and host enable it.

Schema3 upgrades v1/v2 only after SQLite-generated consistent backups, including committed WAL pages, are checked and fsynced. Launch locks and original backup data survive. A reader rejects a newer schema. No raw input/credentials are stored in these catalog documents.

Headless validation covers deterministic invariant sequences, cross-workspace CAS rollback, saved50undo/redo after reopen, and a real shell unchanged across60 transfers plus host restart. This does not certify drag/drop, accessibility, scale or the finished application hierarchy.
