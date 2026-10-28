# Keycloak for conde.cloud: the official image with the build-time options baked in, so the pod runs
# `kc.sh start --optimized`. Built for linux/amd64 and pushed as europe-west3-docker.pkg.dev/conde-prod/conde/keycloak.
FROM quay.io/keycloak/keycloak:26.8.1@sha256:a78d6b79572168cb25886b80a8b5b8d4ed4390743e3eecb810538683e209dd6b AS builder

# cache and cache-stack are run-time options in 26.8 (the build ignores them); the Deployment sets them. The file vault
# is build-time; its directory (KC_VAULT_DIR) is set by the Deployment.
ENV KC_DB=postgres \
    KC_HEALTH_ENABLED=true \
    KC_METRICS_ENABLED=true \
    KC_VAULT=file

# The conde login and account themes (design B sign-in pages and account console) and the demo invitation email
# theme; realms pick them by name.
COPY themes/ /opt/keycloak/themes/

RUN /opt/keycloak/bin/kc.sh build

FROM quay.io/keycloak/keycloak:26.8.1@sha256:a78d6b79572168cb25886b80a8b5b8d4ed4390743e3eecb810538683e209dd6b

COPY --from=builder /opt/keycloak/ /opt/keycloak/

ENTRYPOINT ["/opt/keycloak/bin/kc.sh"]
CMD ["start", "--optimized"]
