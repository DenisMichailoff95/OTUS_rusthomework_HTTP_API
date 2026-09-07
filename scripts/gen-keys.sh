#!/bin/bash
# Генерация ключей JWT для сервиса Shorty
# Использует RS256 (асимметричная подпись)

set -e

mkdir -p keys

# Генерация приватного ключа RS256
openssl genpkey -algorithm RSA -out keys/jwt_private.pem -pkeyopt rsa_keygen_bits:2048

# Извлечение публичного ключа
openssl rsa -in keys/jwt_private.pem -pubout -out keys/jwt_public.pem

echo "✅ JWT ключи сгенерированы:"
echo "   - Приватный: keys/jwt_private.pem"
echo "   - Публичный: keys/jwt_public.pem"
echo ""
echo "⚠️  Никогда не коммитьте приватный ключ в репозиторий!"