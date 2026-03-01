#!/bin/bash
set -e

# Maia SSL Setup Script
# This script sets up Let's Encrypt SSL certificates using certbot

DOMAIN=${1:-""}
EMAIL=${2:-""}

if [ -z "$DOMAIN" ] || [ -z "$EMAIL" ]; then
    echo "Usage: $0 <domain> <email>"
    echo "Example: $0 maia.example.com admin@example.com"
    exit 1
fi

echo "Setting up SSL for $DOMAIN..."

# Create directories
mkdir -p nginx/certs
mkdir -p nginx/certbot

# Create temporary nginx config for initial cert acquisition
cat > nginx/nginx-initial.conf << 'NGINX_EOF'
events {
    worker_connections 1024;
}

http {
    server {
        listen 80;
        server_name _;

        location /.well-known/acme-challenge/ {
            root /var/www/certbot;
        }

        location / {
            return 200 'Waiting for SSL setup...';
            add_header Content-Type text/plain;
        }
    }
}
NGINX_EOF

# Stop existing containers
docker compose down 2>/dev/null || true

# Start nginx with initial config for certbot challenge
echo "Starting nginx for certificate challenge..."
docker run -d --name nginx-certbot-temp \
    -v "$(pwd)/nginx/nginx-initial.conf:/etc/nginx/nginx.conf:ro" \
    -v "$(pwd)/nginx/certbot:/var/www/certbot" \
    -p 80:80 \
    nginx:alpine

# Wait for nginx to start
sleep 3

# Run certbot to obtain certificate
echo "Obtaining certificate from Let's Encrypt..."
docker run --rm \
    -v "$(pwd)/nginx/certs:/etc/letsencrypt" \
    -v "$(pwd)/nginx/certbot:/var/www/certbot" \
    certbot/certbot certonly \
    --webroot \
    --webroot-path=/var/www/certbot \
    --email "$EMAIL" \
    --agree-tos \
    --no-eff-email \
    -d "$DOMAIN"

# Stop temporary nginx
docker stop nginx-certbot-temp
docker rm nginx-certbot-temp

# Create symlinks for certificate files
echo "Setting up certificate symlinks..."
ln -sf "/etc/letsencrypt/live/$DOMAIN/fullchain.pem" nginx/certs/fullchain.pem 2>/dev/null || \
    cp "nginx/certs/live/$DOMAIN/fullchain.pem" nginx/certs/fullchain.pem

ln -sf "/etc/letsencrypt/live/$DOMAIN/privkey.pem" nginx/certs/privkey.pem 2>/dev/null || \
    cp "nginx/certs/live/$DOMAIN/privkey.pem" nginx/certs/privkey.pem

# Update nginx config with actual domain
sed -i.bak "s/server_name _;/server_name $DOMAIN;/g" nginx/nginx.conf

# Clean up
rm -f nginx/nginx-initial.conf

echo ""
echo "SSL setup complete!"
echo ""
echo "Next steps:"
echo "1. Make sure your domain ($DOMAIN) points to this server"
echo "2. Run: docker compose up -d"
echo "3. Access your app at: https://$DOMAIN"
echo ""
echo "To renew certificates (run periodically):"
echo "  ./scripts/renew-ssl.sh"
