import { defineConfig, type DefaultTheme } from 'vitepress'

const base = process.env.DOCS_BASE ?? '/'
if (!/^\/(?:[\w.-]+\/)*$/.test(base)) {
  throw new Error('DOCS_BASE must be an absolute path with a trailing slash')
}

/** Give each audience its own reading order and sidebar. */
function navigation(ru: boolean) {
  const p = ru ? '/ru/' : '/'
  const item = (path: string, en: string, translated: string) => ({
    text: ru ? translated : en,
    link: p + path,
  })
  const install = [
    item('guide/introduction', 'About Shprotify', 'О Shprotify'),
    item('guide/quick-start', 'Quick start', 'Быстрый старт'),
    item('guide/configuration', 'Settings and file locations', 'Настройки и файлы'),
    item('guide/library', 'Add music', 'Добавление музыки'),
    item('guide/accounts', 'Users and access', 'Пользователи и доступ'),
  ]
  const manage = [
    item('guide/proxy', 'HTTPS with Traefik', 'HTTPS с Traefik'),
    item('guide/metadata', 'Tags and covers', 'Теги и обложки'),
    item('guide/discovery', 'Find music', 'Поиск новой музыки'),
    item('guide/downloads', 'YouTube downloads', 'Загрузка с YouTube'),
    item('guide/automation', 'Scheduling', 'Автоматизация'),
    item('guide/operations', 'Backups and updates', 'Бэкапы и обновления'),
    item('guide/deployment', 'Image tags and Docker commands', 'Теги и команды Docker'),
    item('guide/troubleshooting', 'Troubleshooting', 'Решение проблем'),
  ]
  const clientDevelopment = [
    item('clients/overview', 'First API requests', 'Первые запросы'),
    item('clients/authentication', 'Authentication and errors', 'Авторизация и ошибки'),
    item('clients/playback', 'Catalog and playback', 'Каталог и плеер'),
    item('clients/collections', 'Collections and history', 'Коллекции и история'),
    item('clients/discovery', 'Discovery and downloads', 'Рекомендации и загрузки'),
    item('reference/api', 'API reference', 'Справочник API'),
  ]
  const contribute = [
    item('reference/development', 'Contributing', 'Участие в проекте'),
    item('reference/architecture', 'Architecture', 'Архитектура'),
    item('reference/database', 'Database', 'База данных'),
    item('reference/documentation', 'Documentation', 'Документация'),
    item('guide/nix', 'Nix', 'Nix'),
  ]
  const section = (text: string, items: DefaultTheme.SidebarItem[]) => ({
    text, items,
  })
  const clients = [
    section(ru ? 'Клиенты' : 'Clients', [
      item('clients/apps', 'Client apps', 'Клиентские приложения'),
    ]),
    section(ru ? 'Разработка клиента' : 'Building a client', clientDevelopment),
  ]
  const sidebar: DefaultTheme.Sidebar = {
    [p + 'guide/']: [
      section(ru ? 'Начало работы' : 'Getting started', install),
      section(ru ? 'Управление сервером' : 'Server management', manage),
      section(ru ? 'Другие способы установки' : 'Other installation methods', [
        item('guide/nix', 'Nix', 'Nix'),
      ]),
    ],
    [p + 'clients/']: clients,
    [p + 'reference/api']: clients,
    [p + 'reference/']: [section(ru ? 'Для контрибуторов' : 'Contributors', contribute)],
  }
  return {
    nav: [
      item('guide/quick-start', 'Server guide', 'Сервер'),
      item('clients/apps', 'Clients', 'Клиенты'),
      item('reference/api', 'API', 'API'),
      item('reference/development', 'Contributing', 'Разработка'),
    ],
    sidebar,
  }
}

export default defineConfig({
  base,
  title: 'Shprotify',
  description: 'Self-hosted music server. Installation and API documentation.',
  lang: 'en',
  cleanUrls: false,
  appearance: 'dark',
  ignoreDeadLinks: false,
  vite: {
    plugins: [{
      name: 'shprotify:local-search-hmr',
      apply: 'serve',
      enforce: 'pre',
      resolveId(id) {
        // VitePress 1.6 resolves the bare import but accepts HMR by URL.
        // Point that URL at the same virtual module for Vite 6.
        if (id === '/@localSearchIndex') return id
      },
    }],
  },
  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: base + 'logo.svg' }],
    ['meta', { name: 'theme-color', content: '#121212' }],
  ],
  locales: {
    root: {
      label: 'English', lang: 'en',
      themeConfig: {
        ...navigation(false),
        outline: { level: [2, 3], label: 'On this page' },
        footer: { message: 'Shprotify documentation', copyright: 'GPL-3.0-only' },
      },
    },
    ru: {
      label: 'Русский', lang: 'ru',
      description: 'Музыкальный сервер Shprotify. Установка и документация API.',
      themeConfig: {
        ...navigation(true),
        outline: { level: [2, 3], label: 'На этой странице' },
        docFooter: { prev: 'Назад', next: 'Далее' },
        sidebarMenuLabel: 'Навигация',
        returnToTopLabel: 'Наверх',
        skipToContentLabel: 'К содержимому',
        langMenuLabel: 'Язык',
        darkModeSwitchLabel: 'Тема',
        lightModeSwitchTitle: 'Светлая тема',
        darkModeSwitchTitle: 'Тёмная тема',
        editLink: {
          pattern: 'https://github.com/MOIS3Y/shprotify/edit/main/docs/:path',
          text: 'Изменить на GitHub',
        },
        footer: { message: 'Документация Shprotify', copyright: 'GPL-3.0-only' },
      },
    },
  },
  themeConfig: {
    logo: '/logo.svg',
    siteTitle: 'Shprotify',
    socialLinks: [{ icon: 'github', link: 'https://github.com/MOIS3Y/shprotify' }],
    editLink: {
      pattern: 'https://github.com/MOIS3Y/shprotify/edit/main/docs/:path',
      text: 'Edit on GitHub',
    },
    search: {
      provider: 'local',
      options: {
        locales: {
          ru: {
            translations: {
              button: { buttonText: 'Поиск', buttonAriaLabel: 'Поиск по документации' },
              modal: {
                noResultsText: 'Ничего не найдено',
                resetButtonTitle: 'Сбросить поиск',
                displayDetails: 'Показать подробности',
                footer: { selectText: 'выбрать', navigateText: 'переместиться', closeText: 'закрыть' },
              },
            },
          },
        },
      },
    },
  },
  markdown: { lineNumbers: false },
})
