#!/bin/bash
# Генерация ключей JWT для сервиса Shorty
# Использует ES256 (асимметричная подпись)

set -e

mkdir -p keys

# Генерация приватного ключа ES256
openssl ecparam -name prime256v1 -genkey -noout -out keys/jwt_private.pem

# Извлечение публичного ключа
openssl ec -in keys/jwt_private.pem -pubout -out keys/jwt_public.pem

echo "✅ JWT ключи сгенерированы:"
echo "   - Приватный: keys/jwt_private.pem"
echo "   - Публичный: keys/jwt_public.pem"
echo ""
echo "⚠️  Никогда не коммитьте приватный ключ в репозиторий!"