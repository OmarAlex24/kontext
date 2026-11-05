# Inventory

- **Read when:** a product shows as available but the warehouse has none.

Every product has an on-hand count per warehouse. An order reserves units
when it is paid and releases them if it is cancelled.

## Restocking

The warehouse team uploads a count every morning; the import overwrites the
on-hand numbers.
