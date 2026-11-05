# ADR-0001 Server-side sessions in cookies

**Status:** Accepted

## Context

Shoppers log in to see their orders. We need to remember who is logged in
between requests.

## Decision

Keep a session row per login in the database and send its id in an
HTTP-only cookie.
