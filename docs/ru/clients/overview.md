# Первые запросы к API

Сделаем то, что понадобится каждому плееру: подключимся к серверу,
войдём и прочитаем первые байты трека. Для примера нужны Bash, `curl`,
аккаунт и хотя бы один добавленный аудиофайл.
[Подготовить тестовый сервер](../guide/quick-start.md)

## 1. Проверьте адрес сервера

Укажите адрес, который пользователь ввёл бы в плеере, и добавьте
`/api/v1` один раз при настройке HTTP-клиента.

```sh
SERVER=http://localhost:3000
API="$SERVER/api/v1"
curl -fsS "$API/capabilities"
```

Этот запрос доступен без входа. В ответе есть `api_version`,
`server_version`, видимость каталога и ограничения для страниц,
пакетных запросов и коллекций. По `downloads_enabled` и
`automation_enabled` можно решить, какие действия показывать в клиенте.

## 2. Войдите и проверьте аккаунт

Вставьте строку JSON с вашим именем и паролем, например
`{"name":"alice","password":"ваш-пароль"}`. Bash прочитает её без
отображения в терминале.

```sh
read -rsp 'Login JSON: ' login_json
printf '\n'
printf '%s' "$login_json" | curl -fsS "$API/auth/login" \
  -H 'Content-Type: application/json' --data-binary @-
unset login_json
```

Скопируйте значение `access_token` из ответа. Для следующих примеров
вставьте его в ответ на запрос `Token:`.

```sh
read -rsp 'Token: ' ACCESS_TOKEN
printf '\n'
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" "$API/me"
```

Теперь вы знаете, кто вошёл и какая у него роль. Если пришёл `401`,
проверьте адрес сервера и токен. Клиенту стоит сохранить
`expires_at`, чтобы знать, когда понадобится повторный вход.
[Как работать с сессиями и ошибками](./authentication.md)

## 3. Найдите запись и выберите файл

```sh
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" \
  "$API/recordings?limit=20&offset=0"
```

Выберите запись из `items`. Если `has_more` равно `true`, для следующей
страницы увеличьте `offset` на размер прочитанной страницы. Пустой каталог
можно наполнить [сканированием папки](../guide/library.md).

Подставьте ID записи вместо `12`:

```sh
RECORDING_ID=12
curl -fsS -H "Authorization: Bearer $ACCESS_TOKEN" \
  "$API/recordings/$RECORDING_ID/files"
```

В `items` найдите файл, формат которого понимает ваш плеер. Для запроса
аудио нужны два ID: записи и выбранного файла.

## 4. Проверьте поток

Подставьте ID файла вместо `7`. Запрос прочитает до 1024 байт и покажет
заголовки ответа.

```sh
FILE_ID=7
curl -fsS -D - -o /dev/null \
  -H "Authorization: Bearer $ACCESS_TOKEN" -H 'Range: bytes=0-1023' \
  "$API/recordings/$RECORDING_ID/files/$FILE_ID/stream"
```

Для подходящего файла ожидайте `206 Partial Content` и
`Content-Range`. Теперь тот же поток можно передать аудиодвижку.
Для HTML-аудио есть [временная ссылка](./playback.md#browser-playback).

## Что добавить дальше

- [Авторизация](./authentication.md) для входа, выхода и обработки ошибок.
- [Каталог и воспроизведение](./playback.md) для поиска, перемотки и обложек.
- [Личные коллекции](./collections.md) для избранного, очереди и истории.
- [Рекомендации и загрузки](./discovery.md) для поиска новой музыки.

Точные схемы запросов и ответов собраны в [справочнике API](../reference/api.md).
