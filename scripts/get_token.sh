#!/usr/bin/env bash
set -euo pipefail

# Mint a short-lived token for the local Docker integration-test user.
# Tandoor 2.7 removed /api-token-auth/, so local tests create the same
# oauth2_provider.AccessToken record that the former endpoint created.

COMPOSE_FILE="${1:-docker-compose.yml}"

OUTPUT=$(docker compose -f "$COMPOSE_FILE" exec -T web_recipes \
  /opt/recipes/venv/bin/python /opt/recipes/manage.py shell -c "
import uuid
from django.contrib.auth import get_user_model
from django.utils import timezone
from oauth2_provider.models import AccessToken

user = get_user_model().objects.get(username='admin')
token = AccessToken.objects.create(
    user=user,
    token='tda_' + str(uuid.uuid4()).replace('-', '_'),
    expires=timezone.now() + timezone.timedelta(days=1),
    scope='read write app',
)
print('LOCAL_TEST_TOKEN=' + token.token)
")

TOKEN=$(printf '%s\n' "$OUTPUT" | sed -n 's/^LOCAL_TEST_TOKEN=//p' | tail -n 1)
if [ -z "$TOKEN" ]; then
  echo "Token minting did not return a token. Django shell output:" >&2
  printf '%s\n' "$OUTPUT" >&2
  exit 1
fi

echo "$TOKEN"
