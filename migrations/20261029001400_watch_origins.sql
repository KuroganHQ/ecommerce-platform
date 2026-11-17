SET LOCAL lock_timeout = '5s';

ALTER TABLE flow_watches ADD COLUMN origin text CHECK (length(origin) <= 300);
