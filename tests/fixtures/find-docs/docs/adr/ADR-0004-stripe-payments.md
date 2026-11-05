# ADR-0004 Stripe for card payments

**Status:** Accepted

## Decision

Card payments go through Stripe Checkout. The webhook that marks an order as
paid is in `src/payments/stripe.ts`.

## Consequences

Refunds are issued from the Stripe dashboard and synced back by the same
webhook.
