# Pinned SearXNG release. Search engines change often; move this tag forward
# regularly and redeploy (see README.md).
FROM docker.io/searxng/searxng:2026.10.16-8252c0bf6
# The entrypoint expects its config to belong to the image's own user.
COPY --chown=searxng:searxng settings.yml /etc/searxng/settings.yml
USER searxng
