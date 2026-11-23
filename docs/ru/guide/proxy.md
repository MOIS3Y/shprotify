# HTTPS с Traefik

Подключим Shprotify к уже работающему Traefik. Плееры будут обращаться
к серверу по HTTPS на вашем домене, а Traefik передаст запросы в контейнер
Shprotify по общей сети Docker.

## 1. Проверьте, что всё готово

Shprotify должен запускаться по [быстрому старту](./quick-start.md).
В этом примере используются такие настройки Traefik:

| Что понадобится | Значение в примере |
| --- | --- |
| Домен, который указывает на сервер с Traefik | `music.example.org` |
| Docker-сеть, к которой подключён Traefik | `proxy` |
| Точка входа HTTPS (`entryPoint`) | `websecure` |
| Настроенный выпуск сертификатов (`certResolver`) | `letsencrypt` |

У Traefik должен быть включён Docker provider. Проверить сеть и увидеть
подключённые контейнеры можно командой `docker network inspect proxy`.
Настройка самого прокси описана в [руководстве Traefik для Docker](https://doc.traefik.io/traefik/reference/install-configuration/providers/docker/).

## 2. Подключите Shprotify к общей сети

Ниже готовый `compose.yaml` на основе быстрого старта. Замените
`music.example.org` в правиле `Host` своим доменом. Если сеть называется
иначе, замените `proxy` во всех трёх местах. При необходимости укажите
свои имена точки входа и обработчика сертификатов.

<<< ../../public/examples/compose.traefik.yaml{yaml}

> [!TIP]
> Если у вас уже есть свой `compose.yaml`, перенесите из примера блоки
> `labels` и `networks`. Сохраните свои папки и настройки, в том числе
> подключение TOML, если вы им пользуетесь. Уберите блок `ports`, чтобы
> обращаться к Shprotify через Traefik.

Shprotify по-прежнему слушает `0.0.0.0:3000` внутри контейнера.
Traefik обращается к этому порту по сети `proxy`, поэтому публиковать
порт 3000 на хосте не нужно. Лейблы в примере настраивают маршрут
для всего домена, включая пути `/api/v1/` и `/docs`.
[Описание лейблов Traefik](https://doc.traefik.io/traefik/reference/routing-configuration/other-providers/docker/)

## 3. Проверьте HTTPS

Из папки с `compose.yaml` примените изменения и проверьте свой домен:

```sh
docker compose up -d
curl -fsS https://music.example.org/api/v1/health
```

Должен прийти ответ `{"status":"ok"}`. В плеере укажите
`https://music.example.org` как адрес сервера. Если Swagger включён,
он будет доступен по адресу `https://music.example.org/docs`.

## Если веб-плеер на другом домене {#если-веб-плеер-на-другом-домене}

Если страница плеера находится на `https://player.example.org`, добавьте
этот адрес в настройки CORS сервера. Так браузер сможет отправлять
запросы к Shprotify со страницы плеера.

::: code-group
```yaml [Compose]
# В блоке environment сервиса shprotify
SHPROTIFY_HTTP__CORS_ORIGINS: '["https://player.example.org"]'
```
```toml [TOML]
[http]
cors_origins = ["https://player.example.org"]
```
:::

Укажите адрес со схемой и портом, если он нестандартный. Путь и
завершающий слеш опустите. В TOML добавьте строку в существующую секцию
`[http]`. Изменения в Compose применяются через `docker compose up -d`,
а для правок только в TOML достаточно `docker compose restart shprotify`.

> [!NOTE]
> В ссылках на аудио может быть временный ключ `ticket`. Если Traefik
> пишет в журнал запросов полный URL, отключите этот журнал для маршрута
> Shprotify или настройте его так, чтобы ключи и заголовок `Authorization`
> не сохранялись. [Журналы Traefik](https://doc.traefik.io/traefik/observe/logs-and-access-logs/)

## Если не работает

| Что вы видите | С чего начать |
| --- | --- |
| `404` от Traefik | Проверьте домен в `Host`, имя точки входа и лейбл `traefik.enable`. |
| `502 Bad Gateway` | В выводе `docker network inspect proxy` найдите оба контейнера. Убедитесь, что Shprotify запущен и слушает `0.0.0.0:3000`. |
| Ошибка сертификата | Проверьте DNS домена, имя `certResolver` и журнал Traefik. |
| Ошибка CORS в браузере | Сверьте адрес страницы плеера с `http.cors_origins`, включая схему и порт. |

Журнал Shprotify можно открыть командой `docker compose logs --tail 50 shprotify`.
