SET LOCAL lock_timeout = '5s';

CREATE TABLE link_grants (
    token_hash       bytea NOT NULL CHECK (length(token_hash) = 32),
    tenant_id        uuid NOT NULL REFERENCES platform.tenants (id),
    scope            text NOT NULL CHECK (scope IN ('order_read', 'withdrawal_request')),
    order_id         uuid,
    link_hash        bytea,
    origin           text NOT NULL CHECK (length(origin) <= 300),
    generation       bigint NOT NULL CHECK (generation >= 0),
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, token_hash),
    FOREIGN KEY (tenant_id, link_hash) REFERENCES order_tokens (tenant_id, token_hash)
        ON DELETE CASCADE,
    CHECK ((scope = 'order_read') = (order_id IS NOT NULL)),
    CHECK ((scope = 'order_read') = (link_hash IS NOT NULL))
);
CREATE INDEX link_grants_link ON link_grants (tenant_id, link_hash) WHERE link_hash IS NOT NULL;

ALTER TABLE link_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE link_grants FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON link_grants TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE, DELETE ON link_grants TO app_runtime;
