use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::Command;
use xrat_sdk::{config::parse_link, engines::xray};
use serde::Deserialize;
use std::time::Duration;
use std::collections::HashMap;

struct XrayChild(tokio::process::Child);
impl Drop for XrayChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// Структура для одного ключа внутри top10
#[derive(Debug, Deserialize)]
struct VlessKeyInfo {
    key: String, // Сама VLESS-ссылка
    #[serde(default)]
    latency_ms: Option<f64>, // Задержка может отсутствовать
}

// Структура для информации о регионе
#[derive(Debug, Deserialize)]
struct RegionInfo {
    best: Option<String>, // Лучшая ссылка, может быть null
    #[serde(default)]
    top10: Vec<VlessKeyInfo>, // Массив ключей
}

// Верхнеуровневая структура: карта "регион -> информация"
type VlessData = HashMap<String, RegionInfo>;

/// Загружает и парсит keys.json, возвращая все найденные VLESS-ссылки.
pub async fn fetch_all_vless_links() -> Result<Vec<String>> {
    let url = "https://raw.githubusercontent.com/tiagorrg/vless-checker/main/docs/keys.json";

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let resp = client.get(url).send().await.context("Не удалось загрузить keys.json")?;

    // Парсим как HashMap. Ключи вроде "updated_at" (строка) вызовут ошибку,
    // поэтому используем более гибкий подход: парсим в serde_json::Value,
    // а затем извлекаем нужные поля.
    let raw_json: serde_json::Value = resp.json().await.context("Не удалось разобрать JSON")?;

    let mut links = Vec::new();

    if let Some(obj) = raw_json.as_object() {
        for (region_name, region_value) in obj {
            // Пропускаем служебные поля, которые не являются регионами
            if region_name == "updated_at" { continue; }

            // Пытаемся десериализовать значение как RegionInfo
            if let Ok(region_info) = serde_json::from_value::<RegionInfo>(region_value.clone()) {
                // Добавляем лучший ключ, если он есть
                if let Some(best) = region_info.best {
                    links.push(best);
                }
                // Добавляем все ключи из top10
                for key_info in region_info.top10 {
                    links.push(key_info.key);
                }
            }
        }
    }

    // Удаляем дубликаты, сохраняя порядок
    let mut unique_links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for link in links {
        if seen.insert(link.clone()) {
            unique_links.push(link);
        }
    }

    Ok(unique_links)
}


#[tokio::main]
async fn main() -> Result<()> {
    let all_links = fetch_all_vless_links().await?;
    let link = all_links.first().context("Не удалось получить ни одной VLESS-ссылки")?;
    println!("Используется ссылка: {}", link);

    let node = parse_link(link)?.context("Не удалось разобрать VLESS-ссылку")?;

    // 1. Разбираем ссылку VLESS и генерируем конфиг Xray.
    //    Локальный SOCKS5-прокси будет слушать на порту 1080.
    //let node = parse_link(link)?.context("Не удалось разобрать VLESS-ссылку")?;

    let config = xray::generate_runtime_config(&node, 1080, None)
        .map_err(|e| anyhow::anyhow!("Ошибка генерации конфига Xray: {}", e))?;

    // Превращаем сгенерированный конфиг в промежуточное JSON-дерево,
    // чтобы иметь возможность править отдельные поля.
    let mut config_value = serde_json::to_value(&config)?;

    // Меняем уровень логирования Xray на debug.
    // Если поля log/loglevel вдруг нет — создаём их.
    config_value["log"]["loglevel"] = serde_json::json!("debug");

    // Обратно в красивую строку.
    let config_json = serde_json::to_string_pretty(&config_value)?;
    println!("Сгенерированный конфиг Xray:\n{}", config_json);

    // 2. Сохраняем конфиг во временный файл.
    let config_path = std::env::temp_dir().join("xray_vpn_config.json");
    tokio::fs::write(&config_path, config_json).await?;

    // 3. Запускаем Xray как дочерний процесс.
    //    Предполагается, что бинарник `xray` (или `xray-rs`) доступен в PATH
    //    или указан полный путь к нему.
    let mut child = Command::new("xray")
        .arg("run")
        .arg("-c")
        .arg(&config_path)
        .stdout(Stdio::inherit()) // Пробрасываем логи Xray в консоль
        .stderr(Stdio::inherit())
        .spawn()
        .context("Не удалось запустить процесс Xray. Убедитесь, что бинарник доступен.")?;

    println!("Xray запущен (PID: {:?}). Ожидание инициализации...", child.id());

    // Даём Xray немного времени на запуск и инициализацию прокси-порта.
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    // 4. Используем локальный SOCKS5-прокси для выполнения запроса.
    //    Весь трафик этого клиента пойдёт через VPN.
    let proxy_url = reqwest::Url::parse("socks5h://127.0.0.1:1080")?;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(proxy_url)?)
        .timeout(std::time::Duration::from_secs(25))
        .build()?;

    println!("Попытка выполнить HTTP-запрос через VPN...");
    let response = client.get("https://httpbin.org/ip").send().await;

    match response {
        Ok(resp) => {
            let body = resp.text().await?;
            println!("Ответ от httpbin.org/ip через VPN: {}", body);
        }
        Err(e) => {
            eprintln!("Ошибка при выполнении запроса через прокси: {}", e);
        }
    }

    // 5. Корректно завершаем процесс Xray при выходе.
    println!("Завершение работы. Остановка Xray...");
    child.kill().await.context("Не удалось остановить процесс Xray")?;
    let _ = tokio::fs::remove_file(&config_path).await; // Очистка

    Ok(())
}
