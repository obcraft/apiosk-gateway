-- Provider avatars are already public profile data. The v2 catalog runtime
-- needs this one additional column to return the same source marks as the App.
GRANT SELECT(avatar_url) ON public.provider_profiles TO gateway_v2_runtime;
