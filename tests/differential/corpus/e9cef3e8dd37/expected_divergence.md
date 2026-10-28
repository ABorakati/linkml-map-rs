category: rust-gap: conflicting join specs for one table name are not rejected

Upstream raises `ValueError: Conflicting join specs for 'Order': ('order_ref', 'id') vs ('id', 'id'). Use distinct join names to join a table more than one way.` when a nested class derivation joins `Order` a second way (here through `populated_from: Order.id` on the inner derivation); Rust ignores the conflict and succeeds. Found by the fuzz campaign during the big-integer work; unrelated to it (no numbers are involved) and present on master.
