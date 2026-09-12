import { createDomainFetch } from "./domain-fetch.ts"

globalThis.fetch = createDomainFetch(globalThis.fetch.bind(globalThis), [
  Deno.env.get("SUPABASE_URL") || "",
  "https://api.apiosk.com",
])
