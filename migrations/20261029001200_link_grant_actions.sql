SET LOCAL lock_timeout = '5s';

ALTER TABLE link_grants DROP CONSTRAINT link_grants_scope_check;
ALTER TABLE link_grants
    ADD CONSTRAINT link_grants_scope_check
        CHECK (scope IN ('order_read', 'withdrawal_request', 'withdrawal', 'review')),
    ADD COLUMN action_hash bytea CHECK (length(action_hash) = 32),
    ADD CONSTRAINT link_grants_action_check
        CHECK ((scope IN ('withdrawal', 'review')) = (action_hash IS NOT NULL));
