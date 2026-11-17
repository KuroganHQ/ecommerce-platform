SET LOCAL lock_timeout = '5s';

CREATE FUNCTION platform.is_platform_hostname(p_hostname text) RETURNS boolean
LANGUAGE sql STABLE
SET search_path = ''
AS $$
    SELECT p_hostname LIKE '%.localhost'
        OR EXISTS (SELECT 1 FROM platform.hostname_claims c
                   WHERE c.hostname = p_hostname AND c.platform_issued)
$$;

CREATE FUNCTION platform.refuse_platform_attestation() RETURNS trigger
LANGUAGE plpgsql
SET search_path = ''
AS $$
BEGIN
    IF platform.is_platform_hostname(NEW.hostname) THEN
        RAISE EXCEPTION 'hostname % is a platform hostname; it takes no merchant attestation',
            NEW.hostname
            USING ERRCODE = 'check_violation', CONSTRAINT = 'checkout_origins_platform_attestation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER platform_hostname_takes_no_attestation
    BEFORE INSERT OR UPDATE ON platform.checkout_origins
    FOR EACH ROW WHEN (NEW.history = 'merchant_attested_never_served')
    EXECUTE FUNCTION platform.refuse_platform_attestation();

REVOKE ALL ON FUNCTION platform.is_platform_hostname(text), platform.refuse_platform_attestation()
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.is_platform_hostname(text), platform.refuse_platform_attestation()
    TO app_runtime;
