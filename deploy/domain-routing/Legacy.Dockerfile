# Reuse the verified binaries and retain the legacy gateway's reconciliation supervisor.
ARG GATEWAY_IMAGE
FROM ${GATEWAY_IMAGE}
USER root
RUN mkdir -p /app && ln -s /usr/local/bin/apiosk-payment-executor /app/apiosk-gateway && ln -s /usr/local/bin/apiosk-reconciliation-worker /app/reconciliation_worker
COPY --chmod=0555 legacy-executor/entrypoint.sh /app/entrypoint.sh
WORKDIR /app
USER 10001:10001
CMD ["/app/entrypoint.sh"]
