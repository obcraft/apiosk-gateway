-- Read-only display data for the trusted gateway; no payment or preference writes.
GRANT USAGE ON SCHEMA identity TO gateway_v2_runtime;
GRANT SELECT(user_id,display_currency) ON identity.user_preferences TO gateway_v2_runtime;
GRANT SELECT(quote_currency,as_of_date,rate) ON billing.fx_rates TO gateway_v2_runtime;

CREATE POLICY gateway_display_preference_read ON identity.user_preferences
  FOR SELECT TO gateway_v2_runtime
  USING (user_id = nullif(current_setting('apiosk.display_owner',true),'')::uuid);
CREATE POLICY gateway_display_rates_read ON billing.fx_rates
  FOR SELECT TO gateway_v2_runtime USING (true);
