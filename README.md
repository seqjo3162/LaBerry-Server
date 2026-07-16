# LaBerry Server

> 🚀 **LaBerry** — это полнофункциональная платформа для обмена сообщениями в реальном времени с REST API + WebSocket, голосовой/видеосвязью через WebRTC, end-to-end шифрованием (E2EE), AI-ботом и панелью администратора.

---

## 📋 Оглавление

- [Возможности](#-возможности)
- [Для кого этот проект](#-для-кого-этот-проект)
- [Архитектура](#-архитектура)
- [Технологический стек](#-технологический-стек)
- [Быстрый старт](#-быстрый-старт)
- [Настройка окружения](#-настройка-окружения)
- [Развёртывание](#-развёртывание)
- [База данных](#-база-данных)
- [API Reference](#-api-reference)
- [WebSocket Protocol](#-websocket-protocol)
- [Безопасность](#-безопасность)
- [Администрирование](#-администрирование)
- [Для разработчиков](#-для-разработчиков)
- [Структура проекта](#-структура-проекта)
- [Troubleshooting](#-troubleshooting)
- [Лицензия](#-лицензия)

---

## ✨ Возможности

### Коммуникация
- **Реалтайм-чат** через WebSocket с подпиской на комнаты
- **REST API** — аутентификация, пользователи, серверы, каналы, DM, сообщения, файлы, друзья, статус
- **Голосовые/видео каналы** — WebRTC signaling, STUN/TURN интеграция
- **Screenshare** — трансляция экрана через WebRTC в голосовых каналах
- **Embeds** — автоматические превью ссылок (OpenGraph)
- **GIF-поддержка** — вставка GIF в сообщения

### Файлы
- **Загрузка/скачивание** файлов с превью-миниатюрами
- **Range requests** — поддержка докачки для видео/аудио
- **Архивы ZIP** — просмотр содержимого без распаковки
- **Транскодирование изображений** — автоматическое создание превью

### Безопасность
- **E2EE** — end-to-end шифрование (ECDH P-256 + AES-GCM, с X25519-совместимостью)
- **2FA** — двухфакторная аутентификация через email
- **Argon2id** — хеширование паролей
- **JWT (HS256)** — сессионное управление с token version
- **CSRF protection** — защита от межсайтовых подделок
- **GeoIP blocking** — блокировка по CIDR-диапазонам (RIPE)
- **Rate limiting** — ограничение запросов на уровне middleware
- **Security headers** — HSTS, CSP, X-Frame-Options, Referrer-Policy, Permissions-Policy

### AI
- **Gemka III** — AI-бот с интеграцией OpenAI-compatible API
- Поддержка LM Studio / llama.cpp для локального запуска

### Администрирование
- **Web Admin Panel** — локальная панель управления
- **Admin CLI** — команды бан/дебан, удаление контента,purge
- **Модерация** — управление пользователями и контентом

---

## 👥 Для кого этот проект

### 🏢 Для юридических лиц

LaBerry — готовое решение для организации внутренней коммуникации:

- **Self-hosted** — полный контроль над данными, никаких облачных провайдеров
- **GDPR-совместимость** — вы храните данные на своих серверах
- **Кастомизация** — открытый код, возможность доработки под свои нужды
- **Без подписок** — никаких ежемесячных платежей
- **Масштабируемость** — SQLite для малых команд, легко migrate на PostgreSQL

**Типичные сценарии:**
- Корпоративный мессенджер
- Внутренний канал коммуникации
- Платформа для поддержки клиентов
- Коммуникация для образовательных учреждений

### 💼 Для физических лиц

- **Личный сервер** — разверните за 5 минут на VPS
- **Приватность** — ваши данные остаются у вас
- **Бесплатно** — MIT лицензия, никаких скрытых платежей
- **Кроссплатформенность** — работает на любом хостинге с Rust
- **Без рекламы** — никаких трекеров, никаких сборов данных

### 🔧 Для разработчиков

**Хотите помочь проекту?** Мы всегда рады контрибьюторам!

**Что можно сделать:**
- [x] Найти и сообщить об ошибках / багах
- [x] Проверить код на наличие бэкдоров и уязвимостей
- [x] Предложить улучшения и фичи
- [x] Писать тесты
- [x] Улучшать документацию
- [x] Оптимизировать производительность
- [x] Добавлять новые middleware
- [x] Дорабатывать API

**Как начать:**
1. Форкните репозиторий
2. Создайте feature-ветку
3. Внесите изменения
4. Отправьте Pull Request с описанием

**Требования к PR:**
- Код должен компилироваться (`cargo build`)
- Соответствовать стилю проекта
- Содержать описание изменений
- Не ломать существующую функциональность

---

## 🏗️ Архитектура

```
┌─────────────────────────────────────────────────────────┐
│                    Клиент (Browser)                      │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────────┐  │
│  │  app.html │  │  start.html│  │  Admin Center      │  │
│  └──────────┘  └──────────┘  └──────────────────────┘  │
└──────────────────────────┬──────────────────────────────┘
                           │ HTTPS / WSS
┌──────────────────────────▼──────────────────────────────┐
│                    Caddy (Reverse Proxy)                 │
│  • Auto HTTPS (Let's Encrypt)                           │
│  • TLS termination                                      │
│  • Gzip compression                                     │
│  • Security headers                                     │
└──────────────────────────┬──────────────────────────────┘
                           │ HTTP
┌──────────────────────────▼──────────────────────────────┐
│              LaBerry Server (Rust/Axum)                  │
│  ┌─────────────┐  ┌─────────────┐  ┌────────────────┐  │
│  │  REST API   │  │  WebSocket  │  │  File Serving  │  │
│  │  /api/*     │  │  /ws        │  │  /static/*     │  │
│  └─────────────┘  └─────────────┘  └────────────────┘  │
│  ┌──────────────────────────────────────────────────┐   │
│  │              Middleware Stack                     │   │
│  │  Auth → CSRF → GeoIP → Rate Limit → Host Guard  │   │
│  └──────────────────────────────────────────────────┘   │
│  ┌─────────────┐  ┌─────────────┐  ┌────────────────┐  │
│  │   Auth      │  │   DB        │  │    AI Client   │  │
│  │  JWT/Argon2 │  │  SQLite     │  │  OpenAI API    │  │
│  └─────────────┘  └─────────────┘  └────────────────┘  │
└──────────────────────────┬──────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────┐
│                  TURN Server (coturn)                    │
│              WebRTC NAT traversal                        │
└──────────────────────────────────────────────────────────┘
```

---

## 🛠️ Технологический стек

| Компонент | Технология | Версия |
|---|---|---|
| **Язык** | Rust | 2021 edition |
| **Web framework** | Axum | 0.7.9 |
| **База данных** | SQLite (через SQLx) | 0.8 |
| **Пул соединений** | SQLx connection pool | 32 conn |
| **WAL mode** | SQLite WAL | enabled |
| **Аутентификация** | JWT (HS256) + Argon2id | jsonwebtoken 9 |
| **TLS** | rustls (native) | 0.22 |
| **Reverse Proxy** | Caddy | 2.x |
| **TURN Server** | coturn | latest |
| **Асинхронность** | Tokio | 1.37+ |
| **Кэш/State** | DashMap | 5 (concurrent HashMap) |
| **Картинки** | image | 0.25 |
| **E2EE** | x25519-dalek + crypto_box | ECDH P-256 + AES-GCM (X25519 compatible) |
| **AI Integration** | reqwest | 0.12 |
| **GeoIP** | maxminddb | 0.24 |

---

## 🚀 Быстрый старт

### Требования

- **Rust 1.75+** (MSRV) — [установка](https://rustup.rs/)
- **SQLite** — обычно предустановлен
- **Git** — для клонирования

### Установка

```bash
# Клонируем репозиторий
git clone <your-repo-url>
cd LaBerry-Server

# Копируем файл окружения
cp .env.example .env

# Редактируем .env (см. ниже)
```

### Запуск

```bash
# Быстрый запуск (debug)
cargo run -p laberry_server --bin laberry_server_bin

# Или через cd
cd server
cargo run --release

# Сервер запустится на http://127.0.0.1:5001
```

### Первая настройка

```bash
# Генерация хеша пароля для админа
cargo run -p laberry_server --bin laberry_server_bin admin generate-password-hash

# Введите пароль, скопируйте хеш в .env
```

---

## ⚙️ Настройка окружения

### Полный список переменных (.env)

```bash
# ============================================
# База данных
# ============================================
DB_PATH=laberry.db                    # Путь к SQLite базе данных

# ============================================
# JWT / Аутентификация
# ============================================
JWT_SECRET=<32+ chars>                # Секрет для JWT (min 64 chars recommended)
SECRET_KEY=<32+ chars>                # Дублирует JWT_SECRET для совместимости

# ============================================
# Сервер
# ============================================
HOST=0.0.0.0                          # Адрес привязки
PORT=5001                             # Порт

# ============================================
# TLS / HTTPS
# ============================================
TLS_ENABLED=false                     # Включить native TLS
TLS_CERT_PATH=server/messenger.crt    # Путь к TLS сертификату
TLS_KEY_PATH=server/messenger.key     # Путь к TLS ключу
PFX_PATH=server/messenger.pfx         # Альтернатива: PFX файл
PFX_PASSWORD=                         # Пароль от PFX

# ============================================
# TURN Server (WebRTC)
# ============================================
TURN_HOST=localhost
TURN_PORT=3478
TURN_SECRET=<your-secret>            # Secret для TURN REST API

# ============================================
# E2EE (End-to-End Encryption)
# ============================================
E2EE_ENABLED=true                     # Включить E2EE
E2EE_ALG=LB-E2EE-v1                  # Алгоритм шифрования

# ============================================
# Rate Limiting
# ============================================
RATE_LIMIT_MAX=100                     # Максимум запросов
RATE_LIMIT_WINDOW=60                   # Окно в секундах

# ============================================
# Admin Panel
# ============================================
ADMIN_SECRET=<your-secret>            # Секрет для admin panel
LB_ENABLE_ADMIN_PANEL=1               # Включить админ-панель
LB_ADMIN_PASSWORD_HASH=<argon2id>     # Хеш пароля админа

# ============================================
# CORS
# ============================================
ALLOWED_ORIGINS=*                     # Разрешённые origins (через запятую)

# ============================================
# AI Bot (Gemka III)
# ============================================
AI_API_URL=https://api.openai.com/v1  # URL AI API
AI_API_KEY=                           # API ключ
AI_MODEL=gpt-3.5-turbo                # Модель

# ============================================
# Email (2FA)
# ============================================
SMTP_HOST=smtp.gmail.com
SMTP_PORT=587
SMTP_USER=your-email@gmail.com
SMTP_PASSWORD=your-app-password
SMTP_FROM=laberry.notify@gmail.com

# ============================================
# Хранение файлов
# ============================================
STORAGE_PATH=storage                  # Папка для файлов
MAX_UPLOAD_SIZE=104857600             # Макс. размер файла (100 MB)
```

### Генерация безопасных секретов

```bash
# JWT Secret (64 bytes hex)
openssl rand -hex 32

# TURN Secret
openssl rand -hex 32

# Admin Secret
openssl rand -hex 32
```

---

## 🌐 Развёртывание

### Вариант 1: Native TLS

```bash
# Сгенерируйте self-signed сертификат (для тестов)
openssl req -x509 -newkey rsa:4096 -keyout server/messenger.key \
  -out server/messenger.crt -days 365 -nodes

# Установите TLS_ENABLED=true в .env
# Запустите сервер
cargo run --release
```

### Вариант 2: Caddy Reverse Proxy (рекомендуется)

```bash
# Настройте Caddyfile (см. caddy/Caddyfile)
# Укажите домен и пути к сертификатам

# Запустите Caddy
caddy run --config caddy/Caddyfile

# Или через Docker
docker run -d --name caddy \
  -p 80:80 -p 443:443 \
  -v $(pwd)/caddy/Caddyfile:/etc/caddy/Caddyfile \
  -v caddy_data:/data \
  caddy:latest
```

### Вариант 3: Full Deployment (Caddy + TURN + LaBerry)

```bash
# Запуск TURN сервера
cd turn
docker-compose up -d

# Запуск LaBerry
cd ..
cargo run --release

# Запуск Caddy
caddy run --config caddy/Caddyfile
```

### Docker (планируется)

> Dockerfile и docker-compose для production будут добавлены в будущем.

---

## 🗄️ База данных

### Схема (основные таблицы)

#### `users` — Пользователи
```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    email TEXT UNIQUE,
    email_verified INTEGER NOT NULL DEFAULT 0,
    email_pending TEXT,
    password_hash TEXT NOT NULL,
    is_banned INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    token_version INTEGER NOT NULL DEFAULT 1,
    is_2fa_enabled INTEGER NOT NULL DEFAULT 0,
    two_factor_secret_code_hash TEXT,
    two_factor_code_sent_at TEXT,
    public_encryption_key TEXT,
    terms_accepted_at TEXT,
    terms_agreement_version TEXT,
    cookie_consent_status TEXT NOT NULL DEFAULT 'unknown',
    cookie_consent_at TEXT,
    trust_factor INTEGER NOT NULL DEFAULT 100,
    trust_review_status TEXT NOT NULL DEFAULT 'clear',
    trust_review_reason TEXT,
    trust_review_at TEXT
);
```

#### `servers` — Серверы (как в Discord)
```sql
CREATE TABLE servers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    owner_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    is_public INTEGER NOT NULL DEFAULT 1,
    FOREIGN KEY(owner_id) REFERENCES users(id)
);
```

#### `chats` — Каналы/Чаты
```sql
CREATE TABLE chats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,
    server_id INTEGER,
    is_private INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'text',  -- 'text' | 'voice'
    FOREIGN KEY(server_id) REFERENCES servers(id)
);
```

#### `messages` — Сообщения
```sql
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    sender_id INTEGER NOT NULL,
    content TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    FOREIGN KEY(chat_id) REFERENCES chats(id),
    FOREIGN KEY(sender_id) REFERENCES users(id)
);
```

#### `files` — Файлы
```sql
CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    filename TEXT NOT NULL,
    original_name TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    mime_type TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    uploaded_by INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    message_id INTEGER,
    created_at TEXT NOT NULL,
    content_hash TEXT,
    normalized_hash TEXT,
    thumbnail_path TEXT,
    FOREIGN KEY(uploaded_by) REFERENCES users(id),
    FOREIGN KEY(chat_id) REFERENCES chats(id)
);
```

#### Дополнительные таблицы
- `server_members` — участники серверов
- `server_join_requests` — запросы на вступление
- `chat_participants` — участники чатов
- `friends` — список друзей
- `presence` — статусы пользователей
- `dm_conversations` — диалоги
- `dm_messages` — сообщения в ЛС
- `gifs` — GIF-библиотека
- `embeds_cache` — кеш превью ссылок
- `_schema_version` — версия миграций БД

### Миграции

Миграции применяются автоматически при запуске через [`bootstrap.rs`](server/src/db/bootstrap.rs). Каждая миграция отслеживается в таблице `_schema_version`.

---

## 📡 API Reference

### Аутентификация

| Метод | Путь | Описание |
|---|---|---|
| POST | `/api/auth/register` | Регистрация |
| POST | `/api/auth/login` | Вход |
| POST | `/api/auth/logout` | Выход |
| GET | `/api/auth/verify-email` | Подтверждение email |
| POST | `/api/auth/refresh` | Обновление токена |
| GET | `/api/auth/me` | Текущий пользователь |

### Пользователи

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/users/{id}` | Профиль пользователя |
| PUT | `/api/users/profile` | Обновить профиль |
| GET | `/api/users/search` | Поиск пользователей |
| GET | `/api/users/avatar/{id}` | Аватар |
| POST | `/api/users/avatar/upload` | Загрузить аватар |

### Серверы

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/servers` | Список серверов |
| POST | `/api/servers` | Создать сервер |
| GET | `/api/servers/{id}` | Информация о сервере |
| PUT | `/api/servers/{id}` | Обновить сервер |
| DELETE | `/api/servers/{id}` | Удалить сервер |
| GET | `/api/servers/{id}/members` | Участники |
| POST | `/api/servers/{id}/join` | Присоединиться |

### Чаты и сообщения

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/channels` | Каналы сервера |
| POST | `/api/channels` | Создать канал |
| GET | `/api/channels/{id}/messages` | Сообщения |
| POST | `/api/channels/{id}/messages` | Отправить сообщение |
| PUT | `/api/messages/{id}` | Редактировать |
| DELETE | `/api/messages/{id}` | Удалить |

### Друзья

| Метод | Путь | Описание |
|---|---|---|
| GET | `/api/friends` | Список друзей |
| POST | `/api/friends/{id}/add` | Добавить друга |
| DELETE | `/api/friends/{id}` | Удалить друга |
| GET | `/api/friends/requests` | Запросы в друзья |

### 2FA

| Метод | Путь | Описание |
|---|---|---|
| POST | `/api/twofa/initiate` | Инициализировать 2FA |
| POST | `/api/twofa/verify` | Подтвердить 2FA |
| POST | `/api/twofa/disable` | Отключить 2FA |
| POST | `/api/twofa/verify-code` | Проверить код |

### Админ-панель

LaBerry поставляется с опциональной веб-админкой, которая запускается отдельно и по умолчанию доступна только на локальном интерфейсе.

| Метод | Путь | Описание |
|---|---|---|
| GET | `/admin/` | Корень админ-панели |
| GET | `/admin/login` | Страница входа |
| POST | `/admin/logout` | Выход из админ-панели |
| GET | `/admin/users` | Список пользователей |
| POST | `/admin/users/{id}/ban` | Забанить пользователя |
| POST | `/admin/users/{id}/unban` | Разбанить пользователя |
| POST | `/admin/users/{id}/purge` | Удалить пользовательский контент |
| POST | `/admin/users/{id}/ban_forever` | Забанить навсегда |

> Полная документация API: [LaBerry-API.md](LaBerry-API.md)

> Примечание: админ-панель включается через `LB_ENABLE_ADMIN_PANEL=1` или `LB_ADMIN_PASSWORD[_HASH]` и по умолчанию привязывается к `127.0.0.1` / `::1` для безопасности.

---

## 🔌 WebSocket Protocol

### Подключение

```
WS: ws://host:5001/ws
WSS: wss://host/ws
```

### Аутентификация

- JWT через query параметр: `?token=<jwt>`
- Или через заголовок: `Authorization: Bearer <jwt>`

### Формат сообщений

```json
{
    "type": "message_type",
    "data": { ... }
}
```

### Типы сообщений (клиент → сервер)

| Тип | Описание |
|---|---|
| `join` | Присоединиться к комнате |
| `leave` | Покинуть комнату |
| `message` | Отправить сообщение |
| `typing` | Индикатор набора |
| `voice_join` | Войти в голосовой канал |
| `voice_leave` | Покинуть голосовой канал |
| `rtc_signal` | WebRTC signaling |
| `screenshare_start` | Начать трансляцию экрана |
| `screenshare_stop` | Остановить трансляцию |

### Типы сообщений (сервер → клиент)

| Тип | Описание |
|---|---|
| `message` | Новое сообщение |
| `typing` | Кто-то печатает |
| `presence` | Изменение статуса |
| `room_joined` | Успешное присоединение |
| `room_left` | Покидание комнаты |
| `voice_state` | Состояние голосового канала |
| `rtc_signal` | WebRTC сигнал |
| `error` | Ошибка |

### Комнаты (Rooms)

- **Channel**: `channel:{chat_id}` — текстовый канал
- **DM**: `dm:{user_id_1}_{user_id_2}` — личный чат
- **Voice**: `voice:{chat_id}` — голосовой канал

---

## 🔒 Безопасность

### Реализованные меры

| Мера | Реализация |
|---|---|
| **Пароли** | Argon2id (65536 итераций, 4 потока) |
| **Сессии** | JWT HS256 с token_version |
| **CSRF** | Double-submit cookie + middleware |
| **Rate Limiting** | На уровне middleware (100 req/60s default) |
| **GeoIP Blocking** | Блокировка по CIDR (RIPE database) |
| **Host Header** | Проверка Host header (middleware) |
| **Security Headers** | HSTS, CSP, X-Frame-Options, Referrer-Policy |
| **E2EE** | ECDH P-256 + AES-GCM, с X25519-совместимостью |
| **TLS** | rustls native или Caddy reverse proxy |
| **2FA** | Email-based verification |

### E2EE (End-to-End Encryption)

```
Протокол: LB-E2EE-v1

1. Генерация ключей:
   - Клиент генерирует X25519 keypair
   - Public key сохраняется на сервере (encrypted)

2. Шифрование сообщения:
   - Генерируется ephemeral keypair
   - ECDH: shared_secret = sender_private × receiver_public
   - HKDF: encryption_key = HKDF(shared_secret, chat_id)
   - Шифрование: ChaCha20-Poly1305(plaintext, encryption_key)

3. Хранение:
   - На сервере: ciphertext + ephemeral_public_key
   - Без shared_secret невозможно расшифровать
```

### GeoIP Blocking

Проект использует:
- [`assets/delegated-ripencc-latest`](assets/delegated-ripencc-latest) — данные RIPE
- [`assets/custom_blocked_cidr`](assets/custom_blocked_cidr) — пользовательские CIDR

---

## 🛡️ Администрирование

### Admin Panel

Доступ: `/admin-center` (локальный доступ)

**Возможности:**
- Управление пользователями (бан/дебан, удаление)
- Модерация контента (удаление сообщений, файлов)
- Мониторинг сервера (sysinfo)
- Настройка параметров

### Admin CLI

```bash
# Генерация хеша пароля
cargo run -p laberry_server --bin laberry_server_bin admin generate-password-hash

# Другие команды (расширяются)
cargo run -p laberry_server --bin laberry_server_bin admin --help
```

### Мониторинг

- Логи через `tracing` / `tracing-subscriber`
- Panic logs в `panic.log`
- `sysinfo` crate для метрик системы

---

## 👨‍💻 Для разработчиков

### Разработка

```bash
# Клонирование
git clone <repo-url>
cd LaBerry-Server

# Запуск в debug
cargo run -p laberry_server --bin laberry_server_bin

# Запуск в release
cargo run --release -p laberry_server --bin laberry_server_bin

# Проверка кода
cargo clippy
cargo fmt --check

# Сборка
cargo build --release -p laberry_server
```

### Структура проекта

```
LaBerry-Server/
├── Cargo.toml                    # Workspace manifest
├── .env.example                  # Шаблон окружения
├── .env                          # Ваш конфиг (не коммитить!)
├── README.md                     # Этот файл
├── restart-server.bat            # Скрипт перезапуска (Windows)
├── HTTPS-func.bat               # Скрипт TLS (Windows)
│
├── server/                       # Основной сервер
│   ├── Cargo.toml               # Зависимости
│   ├── src/
│   │   ├── main.rs              # Точка входа
│   │   ├── lib.rs               # Module declarations
│   │   ├── server.rs            # Axum app builder, router
│   │   ├── auth.rs              # JWT + Argon2
│   │   ├── tls.rs               # TLS конфигурация
│   │   ├── e2ee.rs              # E2EE логика
│   │   ├── api_error.rs         # Error handling
│   │   ├── ai_client.rs         # AI bot integration
│   │   ├── admin_cli.rs         # Admin CLI commands
│   │   │
│   │   ├── db/
│   │   │   ├── mod.rs           # DB module
│   │   │   ├── bootstrap.rs     # Initial data (global server)
│   │   │   └── schema.rs        # Migrations (753 lines!)
│   │   │
│   │   ├── middleware/
│   │   │   ├── mod.rs
│   │   │   ├── auth_guard.rs    # JWT validation
│   │   │   ├── csrf_guard.rs    # CSRF protection
│   │   │   ├── geo_guard.rs     # GeoIP blocking
│   │   │   ├── host_guard.rs    # Host header check
│   │   │   └── rate_limit.rs    # Rate limiting
│   │   │
│   │   ├── routes/
│   │   │   ├── mod.rs
│   │   │   ├── auth.rs          # Auth endpoints
│   │   │   ├── users/           # User routes
│   │   │   │   ├── mod.rs
│   │   │   │   ├── profile.rs
│   │   │   │   ├── settings.rs
│   │   │   │   └── social.rs
│   │   │   ├── servers.rs       # Server routes
│   │   │   ├── chats.rs         # Chat routes
│   │   │   ├── messages.rs      # Message routes
│   │   │   ├── dms.rs           # DM routes
│   │   │   ├── friends.rs       # Friends routes
│   │   │   ├── presence.rs      # Presence routes
│   │   │   ├── files/           # File routes
│   │   │   │   ├── mod.rs
│   │   │   │   ├── upload.rs
│   │   │   │   └── serve.rs
│   │   │   ├── profile_files.rs # Avatar handling
│   │   │   ├── gifs.rs          # GIF routes
│   │   │   ├── embeds.rs        # Link previews
│   │   │   ├── rtc.rs           # WebRTC signaling
│   │   │   ├── twofa.rs         # 2FA routes
│   │   │   ├── sessions.rs      # Session management
│   │   │   ├── downloads.rs     # Download routes
│   │   │   ├── pages.rs         # Page routes
│   │   │   └── admin_panel/     # Admin routes
│   │   │       ├── mod.rs
│   │   │       ├── users.rs
│   │   │       ├── content.rs
│   │   │       └── servers.rs
│   │   │
│   │   └── ws/
│   │       ├── mod.rs           # Hub + connection management
│   │       ├── chat.rs          # Chat WebSocket
│   │       ├── presence.rs      # Presence WebSocket
│   │       └── friends_events.rs # Friends events
│   │
│   └── static/                  # Frontend
│       ├── app.html              # Main app
│       ├── start.html            # Landing page
│       ├── css/
│       ├── js/
│       │   ├── app.js            # Main app logic
│       │   ├── api.js            # API client
│       │   ├── auth.js           # Auth logic
│       │   ├── websocket-manager.js
│       │   ├── chat.html         # Chat partial
│       │   └── ...
│       └── lang/                 # i18n
│
├── caddy/
│   └── Caddyfile                # Reverse proxy config
│
├── turn/
│   ├── docker-compose.yml       # TURN Docker setup
│   └── turnserver.conf          # TURN config
│
├── assets/
│   ├── delegated-ripencc-latest # GeoIP data
│   └── custom_blocked_cidr      # Custom blocks
│
└── .cargo/
    └── config.toml              # Cargo config
```

### Ключевые компоненты

#### [`server.rs`](server/src/server.rs)
Axum app builder, router setup, middleware chain.

#### [`auth.rs`](server/src/auth.rs)
JWT generation/validation, Argon2 password verification.

#### [`e2ee.rs`](server/src/e2ee.rs)
E2EE encryption: X25519 key exchange, ChaCha20-Poly1305 encryption.

#### [`ws/mod.rs`](server/src/ws/mod.rs)
WebSocket Hub с:
- DashMap для concurrent access
- Bounded channels (128 buffer)
- Idempotent connections
- Voice state management
- Screenshare tracking

#### [`middleware/`](server/src/middleware/)
- [`auth_guard.rs`](server/src/middleware/auth_guard.rs) — JWT validation
- [`csrf_guard.rs`](server/src/middleware/csrf_guard.rs) — CSRF protection
- [`geo_guard.rs`](server/src/middleware/geo_guard.rs) — GeoIP blocking
- [`host_guard.rs`](server/src/middleware/host_guard.rs) — Host header validation
- [`rate_limit.rs`](server/src/middleware/rate_limit.rs) — Rate limiting

### Debugging

```bash
# Включить debug логи
RUST_LOG=debug cargo run --release

# Включить debug WebSocket сообщений
LB_DEBUG_WS=1 cargo run --release

# Включить backtrace
RUST_BACKTRACE=1 cargo run --release
```

### Кодовая база

- **Всего файлов:** ~60+
- **Строк кода:** ~8000+
- **Модулей:** 12+
- **API endpoints:** 50+
- **WebSocket events:** 15+

---

## 🔍 Troubleshooting

### Сервер не запускается

```bash
# Проверьте, что SECRET_KEY установлен (min 32 chars)
echo $SECRET_KEY

# Проверьте порт
netstat -an | findstr :5001

# Проверьте логи
cat panic.log
```

### TLS проблемы

```bash
# Проверьте пути к сертификатам
ls -la server/messenger.crt
ls -la server/messenger.key

# Self-signed для тестов
openssl req -x509 -newkey rsa:4096 \
  -keyout server/messenger.key \
  -out server/messenger.crt \
  -days 365 -nodes
```

### WebSocket не подключается

- Убедитесь, что JWT токен валиден
- Проверьте CORS настройки
- Убедитесь, что WebSocket path `/ws` не заблокирован

### База данных проблемы

```bash
# Проверить SQLite
sqlite3 laberry.db "SELECT * FROM _schema_version ORDER BY version;"

# Восстановить (осторожно!)
rm laberry.db
# Сервер создаст новую БД при запуске
```

### TURN проблемы

```bash
# Проверить TURN
docker-compose -f turn/docker-compose.yml ps

# Перезапустить TURN
docker-compose -f turn/docker-compose.yml restart
```

---

## 📊 Производительность

### Оптимизации

- **LTO** (Link-Time Optimization): `opt-level = 3` + `lto = fat`
- **Codegen**: `codegen-units = 1` для максимальной оптимизации
- **Strip**: `strip = true` для уменьшения бинарника
- **WAL mode**: SQLite WAL для concurrent read/write
- **DashMap**: concurrent HashMap для zero-lock state
- **Bounded channels**: предотвращение memory leaks
- **Gzip**: сжатие в Caddy

### Benchmarks (ориентировочно)

| Метрика | Значение |
|---|---|
| Startup time | < 100ms |
| WebSocket connections | 1000+ |
| Messages/sec | 5000+ |
| Memory per WS conn | ~5KB |
| Binary size (release) | ~15MB |

---

## 📝 Changelog

### Текущая версия: 0.1.0

**Реализовано:**
- ✅ REST API (все основные endpoints)
- ✅ WebSocket real-time chat
- ✅ Voice channels (WebRTC)
- ✅ E2EE (P-256 ECDH + AES-GCM, X25519 compatible)
- ✅ Admin panel (web + CLI)
- ✅ 2FA (email)
- ✅ GeoIP blocking
- ✅ Rate limiting
- ✅ CSRF protection
- ✅ AI bot integration
- ✅ File upload/download
- ✅ GIF support
- ✅ Link embeds
- ✅ Presence system
- ✅ Friends system
- ✅ Sessions management
- ✅ Caddy configuration
- ✅ TURN server setup

---

## 📄 Лицензия

MIT License

См. [`LICENSE`](LICENSE) файл.

---

## 🤝 Вклад

### Как помочь

1. **Найти баг?** — Создайте Issue с описанием
2. **Найти бэкдор?** — Создайте Security Advisory
3. **Есть идея?** — Предложите Feature Request
4. **Хотите код?** — Fork → Branch → PR

### Правила

- Код должен компилироваться (`cargo build`)
- Следуйте стилю проекта (`cargo fmt`)
- Пишите понятные commit messages
- Не коммитьте `.env` файлы

### Контакты

- Issues: [GitHub Issues](https://github.com/your-repo/issues)
- Security: [security@your-domain.com](mailto:security@your-domain.com)

---

## 🙏 Благодарности

- [Axum](https://github.com/tokio-rs/axum) — web framework
- [SQLx](https://github.com/launchbadge/sqlx) — database toolkit
- [rustls](https://github.com/rustls/rustls) — TLS
- [coturn](https://github.com/coturn/coturn) — TURN server
- [Caddy](https://caddyserver.com/) — reverse proxy
- [x25519-dalek](https://github.com/dalek-cryptography/x25519-dalek) — ECDH
- Все контрибьюторы проекта

---

<div align="center">

**LaBerry Server** — Built with ❤️ and 🦀 Rust

[Documentation](README.md) · [API Reference](LaBerry-API.md) · [Issues](https://github.com/your-repo/issues)

</div>
