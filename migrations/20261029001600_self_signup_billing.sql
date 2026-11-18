-- SHOP-45: merchant self-signup and Stripe subscription billing.

-- Set only by self-signup, in the tenant's creation transaction: one self-signed-up store per
-- user. CLI-created tenants leave it NULL.
ALTER TABLE platform.tenants ADD COLUMN signup_user_id text;
CREATE UNIQUE INDEX tenants_signup_user_id ON platform.tenants (signup_user_id);

-- The store's subscription on the platform's Stripe account. The customer id is our mapping
-- from billing events to tenants.
CREATE TABLE platform.subscriptions (
    tenant_id              uuid PRIMARY KEY REFERENCES platform.tenants (id),
    stripe_customer_id     text NOT NULL UNIQUE,
    stripe_subscription_id text UNIQUE,
    price_id               text,
    plan                   text,
    status                 text CHECK (status IN ('incomplete', 'incomplete_expired', 'trialing',
                               'active', 'past_due', 'canceled', 'unpaid', 'paused')),
    current_period_end     timestamptz,
    cancel_at_period_end   boolean NOT NULL DEFAULT false,
    updated_at             timestamptz NOT NULL DEFAULT now()
);
