SET LOCAL lock_timeout = '5s';

CREATE TABLE checkout_placements (
    state_hash           bytea NOT NULL CHECK (length(state_hash) = 32),
    tenant_id            uuid NOT NULL REFERENCES platform.tenants (id),
    cart_id              uuid NOT NULL,
    capability_hash      bytea NOT NULL CHECK (length(capability_hash) = 32),
    origin               text NOT NULL CHECK (length(origin) <= 300),
    generation           bigint NOT NULL CHECK (generation >= 0),
    mode                 text NOT NULL CHECK (mode IN ('isolated', 'same_origin')),
    expected_customer_id uuid NOT NULL,
    input                jsonb NOT NULL,
    expires_at           timestamptz NOT NULL,
    consumed_at          timestamptz,
    as_guest             boolean,
    created_at           timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, state_hash),
    FOREIGN KEY (tenant_id, cart_id) REFERENCES carts (tenant_id, id) ON DELETE CASCADE,
    FOREIGN KEY (tenant_id, expected_customer_id) REFERENCES customers (tenant_id, id)
        ON DELETE CASCADE,
    CHECK ((consumed_at IS NULL) = (as_guest IS NULL))
);
CREATE INDEX checkout_placements_cart ON checkout_placements (tenant_id, cart_id);

ALTER TABLE checkout_placements ENABLE ROW LEVEL SECURITY;
ALTER TABLE checkout_placements FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON checkout_placements TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE, DELETE ON checkout_placements TO app_runtime;
