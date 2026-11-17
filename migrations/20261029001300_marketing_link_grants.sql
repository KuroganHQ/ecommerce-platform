SET LOCAL lock_timeout = '5s';

ALTER TABLE link_grants
    DROP CONSTRAINT link_grants_scope_check,
    DROP CONSTRAINT link_grants_action_check;
ALTER TABLE link_grants
    ADD CONSTRAINT link_grants_scope_check
        CHECK (scope IN ('order_read', 'withdrawal_request', 'withdrawal', 'review',
                         'newsletter_confirmation', 'newsletter_preferences',
                         'watch_confirmation', 'watch_unsubscribe', 'flow_unsubscribe',
                         'restore_cart')),
    ADD CONSTRAINT link_grants_action_check
        CHECK ((scope IN ('order_read', 'withdrawal_request')) = (action_hash IS NULL));
