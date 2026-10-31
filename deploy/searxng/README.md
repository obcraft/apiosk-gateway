# Private SearXNG

The free web and news source (`web-search`) asks this SearXNG first, then the
free Bing News, Google News, DuckDuckGo and Wikipedia endpoints. Without it the
source still answers from those backups.

It runs as its own Fly app with no public address. The gateway reaches it on
Fly's private network.

## First setup

```sh
fly apps create apiosk-searxng
fly secrets set --app apiosk-searxng SEARXNG_SECRET="$(openssl rand -hex 32)"
fly deploy --config deploy/searxng/fly.toml --app apiosk-searxng
fly secrets set --app apiosk-gateway-v2 SEARXNG_URL=http://apiosk-searxng.internal:8080
```

## Updating

Search engines change their pages often. Move the image tag in `Dockerfile` to
a newer `searxng/searxng` release and deploy again with the command above.

## Limits

SearXNG fetches the same public result pages a browser would. Engines may
block a datacenter address after many searches; the source then answers from
the backups, and Tavily remains the paid fallback for web search.
