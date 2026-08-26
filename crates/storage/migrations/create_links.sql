-- Создание таблицы ссылок
CREATE TABLE IF NOT EXISTS links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT UNIQUE NOT NULL,
    target_url TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    hits BIGINT NOT NULL DEFAULT 0,
    
    CONSTRAINT code_length CHECK (char_length(code) BETWEEN 4 AND 32),
    CONSTRAINT target_url_not_empty CHECK (char_length(target_url) > 0)
);

-- Индексы для быстрого поиска по коду
CREATE INDEX IF NOT EXISTS idx_links_code ON links(code);

-- Индекс для листинга (keyset pagination)
CREATE INDEX IF NOT EXISTS idx_links_created_at_id ON links(created_at DESC, id DESC);

-- Индекс для очистки просроченных ссылок
CREATE INDEX IF NOT EXISTS idx_links_expires_at ON links(expires_at) WHERE expires_at IS NOT NULL;

-- Таблица для processed_messages (для future use)
CREATE TABLE IF NOT EXISTS processed_messages (
    message_id UUID PRIMARY KEY,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Таблица для outbox (для future use)
CREATE TABLE IF NOT EXISTS outbox (
    id BIGSERIAL PRIMARY KEY,
    message_id UUID NOT NULL,
    aggregate_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ,
    
    INDEX idx_outbox_published_at (published_at) WHERE published_at IS NULL
);