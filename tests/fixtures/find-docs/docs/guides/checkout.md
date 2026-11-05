# Checkout

- **Read when:** changing the cart, the checkout flow or how coupons apply.

The cart lives in `src/checkout/cart.ts`. Coupons are validated in
`src/checkout/coupons.ts` before the total is computed.

## Steps

1. The shopper reviews the cart.
2. The shop computes taxes and the shipping fee.
3. The shopper pays (see ADR-0004).
