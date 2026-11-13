# ufathom: руководство на русском

ufathom снимает дамп рефлексии Unreal Engine и генерирует по нему Rust SDK. Его можно загрузить в игру как DLL или запустить снаружи, против живого процесса или минидампа. Главное отличие от Dumper-7: неверное смещение даёт сообщение об ошибке, а не упавшую игру.

Это руководство для пользователя. Подробности устройства лежат в [ARCHITECTURE.md](../../ARCHITECTURE.md) (на английском), а список проверенных версий движка — в [COMPATIBILITY.md](../../COMPATIBILITY.md).

> **Статус: пре-релиз.** Целевой диапазон UE 4.11–5.8, но поддержка заявляется только для версий, проверенных на настоящих сборках. Возможности SDK для работы внутри игры (сессия, вызов функций, хук игрового потока) проверены пока только на синтетическом движке в тестах.

## Содержание

1. [Снять дамп](#снять-дамп)
2. [Что лежит в SDK](#что-лежит-в-sdk)
3. [Собрать приложение](#собрать-приложение)
4. [Сессия: имена, объекты, поиск](#сессия-имена-объекты-поиск)
5. [Вызов функций игры](#вызов-функций-игры)
6. [Игровой поток и перехват функций](#игровой-поток-и-перехват-функций)
7. [Строки, имена и массивы](#строки-имена-и-массивы)
8. [Ограничения и безопасность](#ограничения-и-безопасность)
9. [Соответствие Dumper-7](#соответствие-dumper-7)

## Снять дамп

**Проще всего снаружи.** Запустите игру и выполните:

```
ufathom quick
```

Выберите процесс по номеру в списке или по PID. ufathom создаст новую папку и ничего не перезапишет:

```
ufathom-dumps/<время>-p<PID>/
├── dump.json         дамп рефлексии
├── Mappings.usmap    маппинги для просмотрщиков ассетов
└── sdk/              самодостаточный Rust SDK
```

- `ufathom quick --all` показывает все процессы, если игра не попала в список вероятных целей.
- `ufathom quick --output D:/dumps` меняет родительскую папку.

**Через DLL.** Положите рядом с exe игры `ufathom.toml`, загрузите `ufathom.dll` любым загрузчиком и нажмите F8 для дампа или F6 для выгрузки.

```toml
output = "D:/dumps"
formats = ["json", "usmap", "rust-sdk"]
auto_dump = false
```

**Отдельные команды:**

```
ufathom ps                                         # вероятные процессы Unreal Engine
ufathom inspect-exe Game-Win64-Shipping.exe        # статический разбор exe, ничего не запускается
ufathom doctor --process Game-Win64-Shipping.exe   # насколько уверенно найдена раскладка
ufathom dump --process Game-Win64-Shipping.exe -o dump.json --usmap Mappings.usmap
ufathom gen dump.json -o my_sdk/                   # SDK по готовому дампу
ufathom diff old.json new.json                     # что изменилось между сборками
```

Цель задаётся через `--pid N`, `--process ИМЯ` или `--minidump ПУТЬ`. Коды выхода: 0 — успех, 1 — ошибка, 2 — неверные аргументы, 3 — дамп с деградацией.

## Что лежит в SDK

SDK — это обычный Cargo-крейт. Он собирается в любой папке: исходники рантайма лежат в `vendor/`.

- **Типы.** Каждый класс и структура описаны как `#[repr(C)]`, а размеры и смещения полей проверяются при компиляции. Наследование работает через `Deref`, а `IsA<Base>` реализован для всех предков.
- **Пакеты.** Каждый пакет — отдельный модуль за Cargo-фичей с тем же именем. По умолчанию включены все.
- **`TypePath`.** У каждого типа есть путь в движке, например `AActor::PATH == "/Script/Engine.Actor"`. На нём держатся `static_class`, приведение типов и списки объектов.
- **`ENGINE`.** Описание сборки: где лежат таблица имён и массив объектов, нужные смещения, индекс ProcessEvent в vtable, слоты GWorld и GEngine.
- **Функции SDK:** `session()`, а также `gworld()` и `gengine()` с типами `UWorld`/`UEngine`.
- **С фичей `inproc`:** ещё `attach()` и `game_thread()`.
- **Обёртки функций.** Каждый `UFunction` превращается в метод, который принимает `&impl Executor`.
- **`src/main.rs`.** Ваш код. При повторной генерации SDK этот файл не перезаписывается.

## Собрать приложение

Логика пишется в функции `run` в `src/main.rs`. Один и тот же код собирается двумя способами.

**DLL для загрузки в игру:**

```bat
cd sdk
cargo build --release --lib
```

Получится `target/release/ufathom_sdk.dll`. После загрузки `run` один раз запускается в отдельном рабочем потоке с `SelfMemory`. DLL остаётся в памяти и выгрузку не поддерживает.

**Внешний exe**, который только читает память:

```bat
cargo build --release --bin ufathom-sdk-external
.\target\release\ufathom-sdk-external.exe <PID>
```

Снаружи можно только читать. Вызывать функции игры из другого процесса нельзя, и это проверяется при компиляции: у внешнего читателя памяти нет `Executor`.

Для вызова функций включите фичу: `cargo build --release --lib --features inproc`.

## Сессия: имена, объекты, поиск

`sdk::session(memory)` открывает `Session` для этой сборки. Внутри игры передавайте `SelfMemory::new()`, снаружи — `ProcessMemory`. Каждое чтение проверяется, так что устаревший указатель даёт ошибку, а не падение.

```rust
use ufathom_runtime as rt;
use ufathom_sdk as sdk;

let session = sdk::session(memory)?;

// Мир и движок
if let Some(world) = sdk::gworld(&session)? {
    println!("{}", session.full_name_of(world)?);   // "World /Game/Maps/Main.Main"
}

// StaticClass и CDO
let actor_class = session.static_class::<sdk::engine::AActor>()?;
let defaults = session.default_object::<sdk::engine::AActor>()?;

// Все объекты класса, включая подклассы; CDO не входят
for pawn in session.objects_of::<sdk::engine::APawn>()? {
    println!("{}", session.path_of(pawn)?);
}

// Поиск по пути и по имени
let func = session.find_object("/Script/Engine.Actor:K2_DestroyActor")?;
let player = session.find_by_name("PlayerController_0", Some("/Script/Engine.PlayerController"))?;

// Проверки типов
let is_pawn = session.is_a(some_object, actor_class)?;
let as_pawn: Option<rt::Ptr<sdk::engine::APawn>> = session.cast::<sdk::engine::APawn, _>(some_object)?;

// Поля
let pawn_ref = session.object(pawn);
let speed = pawn_ref.get(sdk::engine::APawn::SPEED)?;
```

Что ещё есть в `Session`:
- **Объекты:** `num_objects`, `object_at`, `objects`, `for_each_object`.
- **Сведения об объекте:** `name_of`, `class_of`, `outer_of`, `super_of`, `flags_of`, `index_of`, `is_valid`.
- **Значения:** `fstring`, `fname_text`, `read_array`, `read`.

Поиск работает быстро:
- путь, который уже находили, отдаётся из кэша и при этом каждый раз перепроверяется;
- первый поиск строит индекс «имя → объекты» за один проход массива;
- повторный проход нужен, только если объекта в индексе нет. Например, объект создан позже — тогда индекс заодно обновляется.

## Вызов функций игры

Функции вызываются через ProcessEvent и только изнутри игры, с фичей `inproc`.

```rust
// SAFETY: код выполняется внутри игры и на игровом потоке.
let exec = unsafe { sdk::attach() }?;
let location = sdk::engine::AActor::k2_get_actor_location(actor, &exec)?;
sdk::engine::AActor::k2_destroy_actor(actor, &exec)?;
```

Перед каждым вызовом экзекутор проверяет три вещи:
1. объект жив: он всё ещё лежит в своём слоте `GUObjectArray`;
2. путь указывает именно на `UFunction`;
3. буфер параметров не меньше `PropertiesSize` функции.

Только после этого он берёт ProcessEvent из vtable объекта по индексу, найденному при дампе.

**Static-функции** (`UGameplayStatics`, `UKismet*Library` и т. п.) вызываются без `this`. Обёртка сама находит CDO класса через экзекутор:

```rust
let pc = sdk::engine::UGameplayStatics::get_player_controller(&exec, world, 0)?;
```

Экзекутор нельзя передать в другой поток: он остаётся в том потоке, где его создали. Рабочий поток DLL — не игровой поток, поэтому вызывать функции оттуда напрямую неправильно. Для этого есть хук, о нём следующий раздел.

## Игровой поток и перехват функций

`sdk::game_thread()` ставит хук на ProcessEvent и даёт выполнять код на игровом потоке.

```rust
use std::time::Duration;

// SAFETY: внутри игры; DLL остаётся загруженной, пока стоит хук.
let game = unsafe { sdk::game_thread() }?;

// Выполнить на игровом потоке и дождаться результата
let name = game.call(Duration::from_secs(5), |ctx| {
    let world = ctx.session().gworld().ok().flatten()?;
    ctx.session().full_name_of(world).ok()
})?;

// Выполнить без ожидания
game.run(|ctx| {
    // ctx.executor() передаётся в обёртки, ctx.session() даёт поиск
});

// Перехват функции: до вызова можно читать и менять входные параметры или пропустить оригинал
let id = game.before("/Script/Engine.Actor:ReceiveTick", |ctx| {
    let actor = ctx.object();
    if let Some(params) = ctx.params_as::<sdk::engine::AActor_ReceiveTick_Params>() {
        params.delta_seconds *= 0.5;
    }
})?;

// После вызова: выходные параметры и возвращаемое значение
game.after("/Script/Engine.PlayerController:GetHitResultUnderCursor", |ctx| {
    let _ = ctx.params();
})?;

game.remove(id);
game.uninstall()?;   // вернуть vtable как было
game.rescan()?;      // поставить хук снова, захватив и модули, загруженные позже
```

Как это устроено:
- **Без патча кода.** Меняются только указатели в vtable. При установке хук читает vtable всех живых объектов, а у каждого нативного класса есть CDO, поэтому видны все нативные классы. Самое частое значение в слоте ProcessEvent считается оригиналом, и этот слот атомарно подменяется. Страница становится доступной на запись только на время записи.
- **Выбор игрового потока.** Игровым считается поток, который первым сделает 64 вызова через хук. Вызовы из других потоков проходят насквозь, без колбэков.
- **Защита от рекурсии.** Вызовы, которые делает ваш код (задачи, колбэки, их экзекутор), идут мимо хука.
- **Защита от паник.** Паника в вашем коде перехватывается и не уходит в движок. `call` в этом случае вернёт `HookError::Dropped`.
- **Таймаут.** Если игровой поток не успел, `call` вернёт `HookError::Timeout`, но задача останется в очереди и выполнится позже.

## Строки, имена и массивы

**Входные параметры.** Строку или массив можно передать из памяти Rust: ProcessEvent копирует входные параметры поверхностно и никогда их не освобождает. Это тот же приём, что у Dumper-7 в `FString(const wchar_t*)`.

```rust
let text = rt::FStringBuf::new("Привет");
let name = sdk::engine::UKismetStringLibrary::conv_string_to_name(ctx.executor(), text.as_fstring())?;
println!("{}", ctx.session().fname_text(&name)?);

let ids = rt::TArrayBuf::new(vec![1i32, 2, 3]);
some_wrapper(ctx.executor(), ids.as_tarray())?;
```

Буфер должен жить, пока идёт вызов. `FStringBuf` и `TArrayBuf` **нельзя** передавать как out-параметр (неконстантная ссылка): движок перевыделит такой буфер своим аллокатором. Туда передавайте пустое значение (`rt::FString::default()`).

**Результаты** читаются через сессию:

```rust
let label = sdk::engine::UKismetSystemLibrary::get_display_name(ctx.executor(), object)?;
println!("{}", ctx.session().fstring(&label)?);
let items: Vec<rt::Ptr<sdk::engine::AActor>> = ctx.session().read_array(&array)?;
```

- **`FName` из текста** — через `UKismetStringLibrary::Conv_StringToName`.
- **`FText` в текст** — через `UKismetTextLibrary::Conv_TextToString`.
- **Обратно в текст:** `fname_text` и `fstring`.

## Ограничения и безопасность

- **Память движка не освобождается.** Строки и массивы, которые вернул движок, утекают, как в большинстве модов на Dumper-7. Для освобождения нужны `GMalloc` и индекс `Free` в его vtable. Они различаются между версиями UE, и без проверки на настоящей сборке ошибка означает порчу кучи. Поэтому функции, которые возвращают строки, лучше не вызывать каждый кадр.
- **Первый поиск** строит индекс имён за один проход массива объектов. В большой игре это заметная пауза, поэтому нужные пути лучше найти заранее, например при старте мода.
- **Выгрузка DLL с установленным хуком уронит игру.** Шаблон DLL поэтому не выгружается.
- **Видны только загруженные объекты.** Классы Blueprint, которые ещё не загружены, в дамп не попадут.
- **Античит.** Не загружайте ничего в игру с активным античитом или в онлайн-клиент, где это запрещено. ufathom не обходит античит и не прячется от него.
- **Права.** Сгенерированные SDK и дампы содержат данные игры. Не распространяйте их без права на это.

## Соответствие Dumper-7

| Dumper-7 (C++) | ufathom (Rust) |
|---|---|
| `UObject::GObjects` | `session.objects()`, `for_each_object` |
| `UObject::FindObject<T>(path)` | `session.find_object(path)` |
| `UObject::FindObjectFast(name)` | `session.find_by_name(name, class)` |
| `T::StaticClass()` | `session.static_class::<T>()` |
| `T::GetDefaultObj()` | `session.default_object::<T>()` |
| `obj->IsA(T::StaticClass())` | `session.is_a(obj, class)`, `session.cast::<T, _>(obj)` |
| `obj->GetName()` / `GetFullName()` | `session.name_of(obj)` / `full_name_of(obj)` |
| `FName::ToString()` | `session.fname_text(&name)` |
| `FString::ToString()` | `session.fstring(&s)` |
| `FString(L"text")` | `rt::FStringBuf::new("text").as_fstring()` |
| `UWorld::GetWorld()` | `sdk::gworld(&session)` |
| `UEngine::GetEngine()` | `sdk::gengine(&session)` |
| `obj->Func(args)` | `Class::func(obj, &exec, args)` |
| `UClass::StaticFunc(args)` | `Class::static_func(&exec, args)` |
| хук ProcessEvent (вручную) | `game.before(path, ...)`, `game.after(path, ...)` |
| выполнение на игровом потоке (вручную) | `game.run(...)`, `game.call(timeout, ...)` |
