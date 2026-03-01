#!/bin/bash
set -e

# Maia SSL Certificate Renewal Script
# Run this periodically (e.g., weekly via cron)

echo "Renewing SSL certificates..."

# Renew certificates
docker run --rm \
    -v "$(pwd)/nginx/certs:/etc/letsencrypt" \
    -v "$(pwd)/nginx/certbot:/var/www/certbot" \
    certbot/certbot renew --quiet

# Reload nginx to pick up new certificates
docker compose exec nginx nginx -s reload 2>/dev/null || echo "Nginx reload skipped (not running)"

echo "Certificate renewal complete!"
