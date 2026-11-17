SET LOCAL lock_timeout = '5s';

CREATE TABLE checkout_identities (
    state_hash      bytea NOT NULL CHECK (length(state_hash) = 32),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    cart_id         uuid NOT NULL,
    capability_hash bytea NOT NULL CHECK (length(capability_hash) = 32),
    origin          text NOT NULL CHECK (length(origin) <= 300),
    generation      bigint NOT NULL CHECK (generation >= 0),
    expires_at      timestamptz NOT NULL,
    consumed_at     timestamptz,
    customer_id     uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, state_hash),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE CASCADE,
    CHECK ((consumed_at IS NULL) = (customer_id IS NULL))
);
CREATE INDEX checkout_identities_cart ON checkout_identities (tenant_id, cart_id);

ALTER TABLE checkout_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE checkout_identities FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON checkout_identities TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE, DELETE ON checkout_identities TO app_runtime;
