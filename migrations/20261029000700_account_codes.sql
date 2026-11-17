CREATE TABLE account_codes (
    code_hash       bytea NOT NULL CHECK (length(code_hash) = 32),
    tenant_id       uuid NOT NULL REFERENCES platform.tenants (id),
    market_id       uuid NOT NULL,
    purpose         text NOT NULL CHECK (purpose IN ('identify', 'placement')),
    state_hash      bytea NOT NULL CHECK (length(state_hash) = 32),
    callback_origin text NOT NULL,
    customer_id     uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    consumed_at     timestamptz,
    PRIMARY KEY (tenant_id, code_hash),
    FOREIGN KEY (tenant_id, market_id) REFERENCES markets (tenant_id, id),
    FOREIGN KEY (tenant_id, customer_id) REFERENCES customers (tenant_id, id) ON DELETE CASCADE,
    CHECK (purpose = 'placement' OR customer_id IS NOT NULL)
);
CREATE INDEX account_codes_expiry ON account_codes (expires_at);

ALTER TABLE account_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE account_codes FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON account_codes TO app_runtime
    USING (tenant_id = current_setting('app.tenant_id')::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id')::uuid);
GRANT SELECT, INSERT, UPDATE ON account_codes TO app_runtime;

CREATE OR REPLACE FUNCTION platform.purge_customer_auth() RETURNS bigint
LANGUAGE sql VOLATILE SECURITY DEFINER
SET search_path = ''
AS $$
    WITH sessions AS (
        DELETE FROM public.customer_sessions WHERE expires_at < now() RETURNING 1
    ), links AS (
        DELETE FROM public.customer_magic_links WHERE expires_at < now() - interval '1 day'
        RETURNING 1
    ), attempts AS (
        DELETE FROM public.customer_auth_attempts WHERE at < now() - interval '1 day' RETURNING 1
    ), salts AS (
        DELETE FROM platform.ip_salts WHERE day < (now() AT TIME ZONE 'utc')::date - 1 RETURNING 1
    ), codes AS (
        DELETE FROM public.account_codes WHERE expires_at < now() RETURNING 1
    )
    SELECT (SELECT count(*) FROM sessions) + (SELECT count(*) FROM links)
         + (SELECT count(*) FROM attempts) + (SELECT count(*) FROM salts)
         + (SELECT count(*) FROM codes)
$$;
