# ADR-0003 Signed tokens for login

**Status:** Accepted
Supersedes: ADR-0001

## Context

Session rows in the database became a bottleneck at peak traffic, and the
mobile app cannot rely on cookies.

## Decision

Login returns a short-lived signed token plus a refresh token. The session
check lives in `src/auth/session.ts` and does not touch the database.
