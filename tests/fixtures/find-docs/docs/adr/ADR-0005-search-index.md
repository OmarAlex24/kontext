# ADR-0005 A search index for the catalog

**Status:** draft

## Proposal

Move product search out of SQL `LIKE` queries into a dedicated search index,
rebuilt when a product changes.
