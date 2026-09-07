#!/bin/bash
set -e

if [ ! -f /app/keys/jwt_private.pem ] || [ ! -f /app/keys/jwt_public.pem ]; then
    echo "Generating JWT keys..."
    mkdir -p /app/keys
    openssl genpkey -algorithm RSA -out /app/keys/jwt_private.pem -pkeyopt rsa_keygen_bits:2048
    openssl rsa -in /app/keys/jwt_private.pem -pubout -out /app/keys/jwt_public.pem
    chown -R appuser:appuser /app/keys
fi

exec "$@"
