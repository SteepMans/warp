# Сборка форка под Windows

Форк `SteepMans/warp` собирается как канал **OSS** (`warp-oss.exe`). Этот канал у upstream уже
полностью отделён от официального Warp, поэтому сборка ставится и работает рядом с ним:

| Что | Официальный Warp (Stable) | Наша сборка (OSS) |
|---|---|---|
| Исполняемый файл | `C:\Program Files\Warp\warp.exe` | `%LOCALAPPDATA%\Programs\WarpOss\warp-oss.exe` (установщик) |
| Идентификатор | `dev.warp.Warp` | `dev.warp.WarpOss` |
| Настройки (`settings.toml`) | `%LOCALAPPDATA%\warp\Warp\config` | `%LOCALAPPDATA%\warp\WarpOss\config` |
| Состояние (sqlite, логи) | `%LOCALAPPDATA%\warp\Warp\data` | `%LOCALAPPDATA%\warp\WarpOss\data` |
| Темы, workflows | `%APPDATA%\warp\Warp\data` | `%APPDATA%\warp\WarpOss\data` |
| Скрытые настройки (реестр) | `HKCU\Software\Warp.dev\Warp` | `HKCU\Software\Warp.dev\WarpOss` |
| Домашний каталог (MCP, skills) | `~\.warp` | `~\.warp-oss` |
| Схема URI | `warp://` | `warposs://` |
| Мьютекс single instance | `Local\WarpStable_SingleInstance` | `Local\WarpOss_SingleInstance` |
| AppId установщика | `warp-terminal-stable` | `warp-terminal-oss` |

Вход, AI и Drive в OSS-канале смотрят на продовые сервера Warp (`WarpServerConfig::production()`)
и запуску не мешают: терминал работает без входа.

## Название Ward

Для пользователя сборка называется **Ward**, внутри остаются идентификаторы `WarpOss`, чтобы
существующая установка, её данные и автообновление продолжали работать.

- Видно как Ward: заголовок окна (`crate::features::product_name()`), страница About (сведения о
  форке, ссылка на репозиторий, адрес манифеста и «Check for updates»), кнопка «Update Ward»,
  описание exe в ресурсе версии (`WARP_APP_DISPLAY_NAME`, по умолчанию Ward для
  `CARGO_BIN_NAME=oss`), установщик: `AppName`, запись в «Приложениях», ярлык в «Пуске» и на
  рабочем столе, пункты контекстного меню (`MyAppDisplayName` в `windows-installer.iss`).
  Уведомления Windows берут имя ярлыка с тем же AppUserModelID, поэтому тоже показывают Ward.
- Не меняется: `warp-oss.exe`, `dev.warp.WarpOss`, папки `%LOCALAPPDATA%\warp\WarpOss` и
  `%LOCALAPPDATA%\Programs\WarpOss`, реестр `HKCU\Software\Warp.dev\WarpOss`, схема `warposs://`,
  мьютекс `Local\WarpOss_SingleInstance`, AppId установщика `warp-terminal-oss`, имя
  `WarpOssSetup.exe` и пути в S3, ключи реестра контекстного меню.
- Обновление поверх старой версии: запись удаления та же (AppId не менялся), старый ярлык
  `WarpOss.lnk` в «Пуске» и на рабочем столе удаляется через `[InstallDelete]`. Закреплённый на
  панели задач ярлык Windows не трогаем: он продолжает работать, но может сохранить старую
  подпись, пока его не перезакрепить.
- Аккаунта Warp в OSS нет: страницы Account и Shared blocks скрыты, команды Log out и
  «Open Settings: Account» убраны из палитры, окно входа для функций под логином не
  открывается, экран входа не показывается ни при старте, ни после выхода.

## Зависимости

Проверено на Windows 11, 16 ядер, 32 ГБ ОЗУ.

1. Git for Windows (bash и `patch.exe` нужны build-скриптам).
2. Visual Studio 2022 с MSVC и Windows SDK.
3. Rust: `rustup toolchain install 1.92.0 --profile minimal` (версия из `rust-toolchain.toml`;
   более новый stable тоже стоит, но сборка идёт на 1.92.0).
4. Protobuf: `winget install -e --id Google.Protobuf --scope user`. `protoc.exe` ложится в
   `%LOCALAPPDATA%\Microsoft\WinGet\Packages\Google.Protobuf_Microsoft.Winget.Source_8wekyb3d8bbwe\bin`,
   эту папку нужно добавить в `PATH` (или задать `PROTOC`).
5. Inno Setup 6 (только для установщика): `winget install -e --id JRSoftware.InnoSetup --scope user`,
   `ISCC.exe` в `%LOCALAPPDATA%\Programs\Inno Setup 6`.
6. CMake уже был. LLVM/libclang, gcloud и `script/windows/bootstrap.ps1` под Windows не нужны:
   bindgen используется только в сборке под macOS и в интеграционных тестах.

## Сборка

Профиль `rlto` (release + thin LTO) тот же, что у `script/windows/bundle.ps1`. Отдельные профили
не нужны: и ручная сборка, и установщик, и публикация используют один каталог
`target\x86_64-pc-windows-msvc\rlto`.

```bash
cd /c/work/other/warp
export PATH="$LOCALAPPDATA/Microsoft/WinGet/Packages/Google.Protobuf_Microsoft.Winget.Source_8wekyb3d8bbwe/bin:$PATH"
export CARGO_BIN_NAME=oss WARP_APP_NAME=WarpOss CARGO_FULL_PROFILE=rlto
export GIT_RELEASE_TAG=v0.2026.10.29.00.00.oss_00          # версия для автообновления
export WARP_FORK_RELEASES_URL=https://<s3>/<bucket>/warp-fork   # источник обновлений, необязательно
cargo +1.92.0 build -p warp --profile rlto --bin warp-oss \
  --features release_bundle,gui,nld_classifier_v3,nld_heuristic_v2,autoupdate \
  --target x86_64-pc-windows-msvc
```

Результат: `target\x86_64-pc-windows-msvc\rlto\warp-oss.exe`. Рядом с ним `app/build.rs`
кладёт `conpty.dll` и `OpenConsole.exe`, поэтому exe запускается прямо из `target`.

Установщик (`script\windows\Output\WarpOssSetup.exe`):

```powershell
$env:PATH = "$env:LOCALAPPDATA\Programs\Inno Setup 6;$env:PATH"
.\script\windows\bundle.ps1 -CHANNEL oss -RELEASE_TAG v0.2026.10.29.00.00.oss_00
```

`bundle.ps1 -CHANNEL oss` сам добавляет фичу `autoupdate`, если задан `WARP_FORK_RELEASES_URL`.

Замеры на steep-pc (29.10.2026):

- Первая сборка с нуля: **21 минута** (`Finished rlto profile ... in 20m 58s`).
- `warp-oss.exe` весит **387 МБ** (в профиле `release` включены line tables, `debug = 1`).
- `target` после двух сборок: **~10 ГБ**.
- Финальная линковка с thin LTO берёт до 10 ГБ ОЗУ на один `rustc`. На машине с 32 ГБ при
  открытых IDE и других сессиях вторая сборка упала на линковке с `STATUS_DLL_INIT_FAILED`
  (нехватка памяти), при этом сама библиотека `warp` скомпилировалась. Перед сборкой стоит
  закрыть тяжёлые приложения или ограничить параллелизм: `CARGO_BUILD_JOBS=8`.

### Первый запуск

Свежий профиль показывает онбординг «Welcome to Warp» и пытается сходить на `app.warp.dev`.
Вход не обязателен: после «Get started» (или флага ниже) открывается обычная вкладка с pwsh.
Флаг пройденного онбординга живёт в реестре нашего канала, официальный Warp он не трогает:

```powershell
reg add "HKCU\Software\Warp.dev\WarpOss" /v HasCompletedOnboarding /t REG_SZ /d true /f
```

Запуск из `target` (без установщика) требует, чтобы рядом с exe лежали `conpty.dll`,
`dxcompiler.dll`, `dxil.dll` и `x64\OpenConsole.exe`. `app/build.rs` кладёт их в
`target
lto`, а не в `target_64-pc-windows-msvc
lto`, поэтому их нужно скопировать
(установщик берёт их из `appssets\windowsd` сам):

```bash
cp -r target/rlto/{conpty.dll,dxcompiler.dll,dxil.dll,x64} target/x86_64-pc-windows-msvc/rlto/
```

Важно: если запускать сборку из сессии Claude Code, в окружение Warp попадут переменные
`CLAUDE_CODE_*`, и запущенный внутри `claude` посчитает себя дочерней сессией (не будет вести
транскрипт). Запускать из «чистого» окружения: ярлык, проводник или
`Get-ChildItem Env:CLAUDE* | Remove-Item; Start-Process warp-oss.exe`.

## Восстановление вкладок с Claude Code

Если во вкладке работает Claude Code, после перезапуска или падения сборки вкладка
открывается в той же папке и сама выполняет `claude ... --resume <session-id>` с исходными
ключами запуска.

Как устроено (встроено в штатное восстановление сессий Warp через sqlite):

- **Откуда id.** Claude Code ведёт реестр живых интерактивных процессов
  `~/.claude/sessions/<pid>.json` с полями `sessionId`, `cwd`, `kind`. Файл переписывается при
  смене сессии внутри процесса, проверено: у процесса, запущенного с `-c`, там id текущего
  разговора, а не исходного. Это надёжнее, чем «самый свежий jsonl в папке проекта» (несколько
  claude в одной папке ломают эвристику) и не требует ставить хук `SessionStart` в глобальные
  настройки Claude. Реестр локален для машины, поэтому сессии с других ПК не подхватываются.
- **Как найти процесс вкладки.** `TerminalView` раз в 5 секунд в фоне
  (`app/src/terminal/cli_agent_sessions/claude_resume.rs`, `detect`) берёт pid из реестра и по
  одному снимку таблицы процессов проверяет, что процесс потомок шелла этой вкладки
  (`ShellProcessInfo.pid`). Запуск через раннеры `run-*.ps1|sh` и через хук профиля pwsh тоже
  подходит: важно только родство процессов, а не текст команды.
- **Ключи.** Берутся из командной строки процесса. Сохраняются `--remote-control [имя]`,
  `--model`, `--dangerously-skip-permissions`, `--permission-mode`, `--effort`, `--agent`,
  `--add-dir`, `--mcp-config`, `--settings` и ещё несколько флагов без побочных эффектов.
  Позиционный промпт, всё после `--`, `--resume`, `-c`, `--session-id`, `--name` и неизвестные
  флаги отбрасываются, в конец добавляется `--resume <id>`. Кавычки под pwsh или bash.
- **Когда сохраняется.** При смене найденной сессии (появилась, сменился id, claude завершился)
  вызывается `workspace:save_app`, то есть снимок в sqlite отражает то, что открыто сейчас.
  Закрытая вручную вкладка пропадает из снимка штатно и не восстанавливается.
- **Хранение.** Колонка `terminal_panes.cli_agent_resume_command` (миграция
  `2026-10-29-000000_add_cli_agent_resume_command_to_terminal_panes`), поле
  `TerminalPaneSnapshot::cli_agent_resume_command`. Каталог вкладки в снимке подменяется на `cwd`
  из реестра Claude, потому что `--resume` ищет сессию по папке проекта.
- **Восстановление.** `PaneGroup` при восстановлении вкладки ставит команду в
  `set_pending_command_queue`, тот же механизм, что у команд launch config: команда выполняется
  после bootstrap шелла.

Проверено вживую 29.10.2026 в `C:\work\other\pintest`: claude с
`--remote-control 'warp-fork-restore-test' --model haiku --dangerously-skip-permissions 'Reply with one word: ok'`,
через 5 секунд в sqlite записалось
`claude --remote-control warp-fork-restore-test --model haiku --dangerously-skip-permissions --resume d068312d-...`,
процесс сборки убит (`Stop-Process`, имитация падения), после запуска вкладка открылась в
pintest и продолжила ту же сессию (тот же `sessionId` в реестре, история разговора на экране).

Замечание: при падении сборки процессы claude не убиваются вместе с ней и остаются висеть
(видно и по старой сессии Remote Control). После восстановления их будет два с одним id; старый
нужно закрыть руками. При штатном выходе Warp завершает шеллы сам.

## Автообновление через S3

Штатный механизм Warp (`app/src/autoupdate`, фича cargo `autoupdate`) для канала OSS был
выключен. Теперь:

- `app/src/bin/oss.rs` включает `AutoupdateConfig`, если задан `WARP_FORK_RELEASES_URL`
  (при сборке через `option_env!` или в окружении при запуске, второе удобно для проверки).
- `app/src/autoupdate/fork_manifest.rs`: для канала OSS проверка обновления читает
  `<WARP_FORK_RELEASES_URL>/stable/manifest.json` вместо `channel_versions` сервера Warp:

  ```json
  {"version": "v0.2026.10.29.12.00.oss_00",
   "url": "<WARP_FORK_RELEASES_URL>/stable/v0.2026.10.29.12.00.oss_00/WarpOssSetup.exe",
   "sha256": "<hex>"}
  ```

- Дальше работает штатный цикл: проверка при старте, раз в 10 минут и при активации окна;
  сравнение версий `ParsedVersion` (формат тега `v0.<yyyy>.<MM>.<dd>.<HH>.<mm>.oss_<n>`);
  загрузка установщика (`app/src/autoupdate/windows.rs`), теперь со сверкой sha256 из
  манифеста; установка только когда пользователь жмёт «обновить и перезапустить», тихим
  `WarpOssSetup.exe /SILENT /update=1` в ту же папку.
- Версию сборки задаёт `GIT_RELEASE_TAG`. Без тега автообновление не работает (так у upstream).

Публикация идёт из CI: `.github/workflows/fork_windows.yml` на `windows-latest`. Пуш в
`feature/claude-tab-restore` собирает установщик и кладёт его артефактом сборки; пуш в `master`
дополнительно публикует его в MinIO Дымохода.

| Что | Где |
|---|---|
| Бакет | `warp-fork` в `chimney/minio` (тот же MinIO, что раздаёт SideStore) |
| Манифест | `https://chimney.steep-man.ru/warp-fork/windows/stable/manifest.json` |
| Установщик | `https://chimney.steep-man.ru/warp-fork/windows/stable/<version>/WarpOssSetup.exe` |
| Чтение | анонимно, только `s3:GetObject` (политика бакета) |
| Запись | пользователь `ci-warp`, политика `warp-fork-write`; ключ в секретах `S3_ACCESS_KEY` / `S3_SECRET_KEY` репозитория |
| Ingress | `/warp-fork` на `chimney.steep-man.ru`, манифест в git chimney `k8s/base/warp-fork-ingress.yaml` |

Вручную: `script/windows/publish_s3.ps1 -Bucket warp-fork -Prefix windows` с
`WARP_FORK_RELEASES_URL=https://chimney.steep-man.ru/warp-fork/windows`,
`WARP_FORK_S3_ENDPOINT=https://chimney.steep-man.ru` и ключами `ci-warp` в `WARP_FORK_S3_*`.
Скрипт собирает установщик через `bundle.ps1 -CHANNEL oss`, считает sha256, заливает
установщик, затем `stable/manifest.json` через AWS CLI (MinIO client `mc` больше не раздаётся).
`-DryRun` собирает и считает манифест без заливки.

## Перенос настроек из официального Warp

Официальный Warp (Stable) хранит данные так (проверено на steep-pc):

- `%LOCALAPPDATA%\warp\Warp\config\settings.toml` — все пользовательские настройки (тема,
  шрифт, шелл по умолчанию `new_session_shell_override`, агенты, приватность и так далее).
  Переносимый файл, это главное, что стоит копировать.
- `%LOCALAPPDATA%\warp\Warp\config\keybindings.yaml` — свои сочетания клавиш, если есть.
- `%APPDATA%\warp\Warp\data\themes`, `...\workflows` — свои темы и workflows (сейчас пусто).
- `%LOCALAPPDATA%\warp\Warp\data\warp.sqlite` — окна, вкладки, история блоков, кэш Drive.
  Не копировать: схема у сборок может отличаться, а состояние окон завязано на конкретный
  экземпляр.
- `HKCU\Software\Warp.dev\Warp` — служебные флаги (пройден ли онбординг, счётчики,
  показанные баннеры). Переносить не нужно.
- `~\.warp` — глобальные MCP (`.mcp.json`) и skills, если заведены.

Перенос (только копированием, при закрытой нашей сборке; официальный Warp можно не закрывать,
файлы только читаются):

```powershell
$src = "$env:LOCALAPPDATA\warp\Warp\config"
$dst = "$env:LOCALAPPDATA\warp\WarpOss\config"
New-Item -ItemType Directory -Force $dst | Out-Null
Copy-Item "$src\settings.toml" $dst
if (Test-Path "$src\keybindings.yaml") { Copy-Item "$src\keybindings.yaml" $dst }

# Свои темы и workflows, если появятся:
$srcData = "$env:APPDATA\warp\Warp\data"
$dstData = "$env:APPDATA\warp\WarpOss\data"
New-Item -ItemType Directory -Force $dstData | Out-Null
Copy-Item "$srcData\*" $dstData -Recurse -Force

# Глобальные MCP и skills:
if (Test-Path "$HOME\.warp") { Copy-Item "$HOME\.warp" "$HOME\.warp-oss" -Recurse }
```

После копирования проверить в settings.toml пути, которые указывают на официальную установку
(`C:\Program Files\Warp\...`), и шелл по умолчанию. `start-task-session.ps1` открывает вкладку через `warp://`, то есть в
официальном Warp; хук `Invoke-ClaudeTaskHook` живёт в профиле pwsh и от сборки не зависит
(в нашей сборке с этим хуком не проверялось). Чтобы запускать задачи в нашей сборке, в скрипте нужно
заменить схему на `warposs://action/new_tab?path=...`.
