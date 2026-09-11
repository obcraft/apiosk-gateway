-- Compatibility column required by the existing full-catalog Discover query.
-- Default preserves visibility of already-published endpoints; it does not
-- validate metadata, activate capabilities or change the readiness audit.
ALTER TABLE public.api_endpoints ADD COLUMN IF NOT EXISTS contract_documented boolean NOT NULL DEFAULT true;
