# Установка и запуск через Nix

Если у вас уже есть Nix, можно запустить Shprotify напрямую с GitHub или
собрать его из клона. Из любого источника можно также собрать Docker-образ
локально.

| Что нужно | Как сделать |
| --- | --- |
| Запустить сервер без клона | [Запустить приложение с GitHub](#с-github) |
| Собрать или запустить свою копию исходников | [Работать из клона](#из-клона) |
| Получить образ для Docker Compose | [Собрать образ локально](#образ-docker) |

Нужны возможности `nix-command` и `flakes`. Если они выключены, добавьте
`experimental-features = nix-command flakes` в свой `nix.conf`.
[Настройки Nix](https://nix.dev/manual/nix/latest/command-ref/conf-file.html)

## Подготовьте папку сервера

Для запуска без контейнера выделите папку под музыку, настройки и базу:

```sh
mkdir -p shprotify-server/data shprotify-server/music
cd shprotify-server
```

Сохраните здесь `shprotify.toml`. Пример открывает сервер на этом
компьютере и требует входа для просмотра каталога.

```toml
[http]
bind = "127.0.0.1:3000"

[database]
path = "data/shprotify.db"

[library]
directories = ["music"]
public_catalog = false

[docs]
enabled = true
```

Добавьте файлы в `music/` или укажите абсолютный путь к своей коллекции.
Относительные пути отсчитываются от папки, в которой запускаете сервер.
В следующих командах это `shprotify-server/`.

## Запустите прямо с GitHub {#с-github}

Выполните в папке сервера:

```sh
nix run 'github:MOIS3Y/shprotify/main#shprotify' -- --config shprotify.toml
```

Nix загрузит flake, подготовит пакет и запустит приложение `shprotify`.
Аргументы после `--` получает сам сервер. Для воспроизводимого запуска
замените `main` на нужный тег или хеш коммита.
[Как устроены ссылки на flakes](https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-flake.html)

## Запустите из клона {#из-клона}

Из папки сервера клонируйте репозиторий в соседнюю папку. Запустите то
же приложение из локальных исходников:

```sh
git clone https://github.com/MOIS3Y/shprotify.git ../shprotify-src
nix run ../shprotify-src#shprotify -- --config shprotify.toml
```

Если хотите сначала собрать бинарник, выполните эти команды из папки
сервера. Сборка оставит ссылку `result` в клоне.

```sh
cd ../shprotify-src
nix build .#shprotify
cd ../shprotify-server
../shprotify-src/result/bin/shprotify --config shprotify.toml
```

## Проверьте сервер и создайте администратора

Откройте второй терминал и перейдите в ту же папку `shprotify-server/`.
Проверьте, что сервер отвечает:

```sh
curl -fsS http://localhost:3000/api/v1/health
```

Для команд управления аккаунтами добавьте `shprotify` в `PATH` текущей
оболочки. Выберите тот же источник и версию, что и для запущенного сервера.

::: code-group
```sh [С GitHub]
nix shell 'github:MOIS3Y/shprotify/main#shprotify'
```
```sh [Из клона]
nix shell ../shprotify-src#shprotify
```
:::

В открывшейся оболочке создайте аккаунт, выдайте права и задайте пароль:

```sh
shprotify --config shprotify.toml user create --name alice
shprotify --config shprotify.toml user role --name alice --role admin
shprotify --config shprotify.toml user password --name alice
```

Теперь можно [войти и запустить сканирование](./quick-start.md#воидите).
В пакет входят yt-dlp, FFmpeg, Deno, Chromaprint и сертификаты CA.
Внешние возможности включаются отдельно в [настройках](./configuration.md).

## Соберите Docker-образ локально {#образ-docker}

На Linux выберите источник. Для локальной сборки из клона выполните
команду в папке с `flake.nix`.

::: code-group
```sh [С GitHub]
nix build 'github:MOIS3Y/shprotify/main#dockerImage' -o result-image
```
```sh [Из клона]
nix build .#dockerImage -o result-image
```
:::

В той же папке загрузите собранный архив в Docker:

```sh
docker load -i result-image
```

Docker напечатает имя и тег образа. Nix берёт тег из версии проекта.
Для Compose подготовьте папки и `compose.yaml` из [быстрого старта](./quick-start.md).
Рядом с `compose.yaml` сохраните `.env` с загруженным тегом:

```dotenv
SHPROTIFY_IMAGE=ghcr.io/mois3y/shprotify:0.1.0
```

Запустите Compose с локальным образом, пропустив скачивание из реестра:

```sh
docker compose up -d --pull never
```

Проверьте, что сервер отвечает `{"status":"ok"}`:

```sh
curl -fsS http://localhost:3000/api/v1/health
```

Затем [создайте администратора](./quick-start.md#_4-создаите-администратора)
и добавьте музыку в каталог.

## Соберите сайт и запустите проверки

Эти команды выполняются из клона:

| Команда | Результат |
| --- | --- |
| `nix develop` | Оболочка с инструментами для работы над сервером и документацией. |
| `nix flake check` | Проверки проекта, в том числе тесты сервера, OpenAPI и сборка документации. |
| `nix build .#docs -o result-docs` | Статический сайт с базовым путём `/shprotify/`, как на GitHub Pages. |
| `nix build .#docsRoot -o result-docs-root` | Статический сайт для размещения в корне домена (`/`). |

Сайт для корня можно открыть локально через простой HTTP-сервер:

```sh
python3 -m http.server 4173 --directory result-docs-root
```

Откройте `http://localhost:4173`. Подробнее об инструментах и проверках
в разделе [«Участие в проекте»](../reference/development.md).
