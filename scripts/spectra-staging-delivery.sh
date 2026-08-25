#!/bin/sh
set -eu

host="root@138.197.0.151"
compose="docker compose -f docker-compose.staging.yml --env-file .env"

case "${1:-}" in
  deploy)
    docker build -t spectra-api:staging .
    docker build -t spectra-web:staging web/
    ssh -o BatchMode=yes "$host" \
      "docker image inspect spectra-api:staging >/dev/null && docker tag spectra-api:staging spectra-api:rollback; docker image inspect spectra-web:staging >/dev/null && docker tag spectra-web:staging spectra-web:rollback"
    docker save spectra-api:staging spectra-web:staging | gzip | \
      ssh -o BatchMode=yes "$host" 'gunzip | docker load'
    ssh -o BatchMode=yes "$host" \
      "cd /root/spectra && $compose up -d"
    ;;
  smoke)
    test "$(curl -fsS -o /dev/null -w '%{http_code}' https://spectra.hyborianlabs.net/healthz)" = "200"
    curl -fsS https://spectra.hyborianlabs.net/api/v1/openapi.json >/dev/null
    ssh -o BatchMode=yes "$host" \
      'test "$(docker inspect -f {{.State.Health.Status}} spectra-api-1)" = healthy'
    ;;
  rollback)
    ssh -o BatchMode=yes "$host" \
      "docker image inspect spectra-api:rollback >/dev/null && docker tag spectra-api:rollback spectra-api:staging; docker image inspect spectra-web:rollback >/dev/null && docker tag spectra-web:rollback spectra-web:staging; cd /root/spectra && $compose up -d"
    ;;
  *)
    echo "usage: $0 deploy|smoke|rollback" >&2
    exit 2
    ;;
esac
