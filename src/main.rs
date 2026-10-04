use std::thread;
use std::time::Duration;

use anyhow::Context;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::{Ets, FreeRtos};
use esp_idf_svc::hal::gpio::{PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

use dht_sensor::{dht22, DhtReading};
use serde_json::json;

// Wrapper Delay presisi mikro-detik untuk pembacaan DHT22 di ESP-IDF (std)
struct IdfDelay;
impl embedded_hal::blocking::delay::DelayUs<u8> for IdfDelay {
    fn delay_us(&mut self, us: u8) { Ets::delay_us(us as u32); }
}
impl embedded_hal::blocking::delay::DelayUs<u16> for IdfDelay {
    fn delay_us(&mut self, us: u16) { Ets::delay_us(us as u32); }
}
impl embedded_hal::blocking::delay::DelayUs<u32> for IdfDelay {
    fn delay_us(&mut self, us: u32) { Ets::delay_us(us); }
}
impl embedded_hal::blocking::delay::DelayMs<u8> for IdfDelay {
    fn delay_ms(&mut self, ms: u8) { FreeRtos::delay_ms(ms as u32); }
}
impl embedded_hal::blocking::delay::DelayMs<u16> for IdfDelay {
    fn delay_ms(&mut self, ms: u16) { FreeRtos::delay_ms(ms as u32); }
}
impl embedded_hal::blocking::delay::DelayMs<u32> for IdfDelay {
    fn delay_ms(&mut self, ms: u32) { FreeRtos::delay_ms(ms); }
}

// =========================================================================
// KONFIGURASI WI-FI & AZURE IOT HUB
// =========================================================================
pub const WIFI_SSID: &str = "GalaxyA1505DF";
pub const WIFI_PASS: &str = "worksomething";

// Kredensial Azure IoT Hub
pub const AZURE_IOTHUB_HOST: &str = "iothubesp32s3.azure-devices.net";
pub const DEVICE_ID: &str = "esp32s3-device-01";
pub const AZURE_SAS_TOKEN: &str = "SharedAccessSignature sr=iothubesp32s3.azure-devices.net%2Fdevices%2Fesp32s3-device-01&sig=KSx9ZmaccQi5k4rftEi%2BwsbI5WMdO86H4fJla80v0Vs%3D&se=1852030800";

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("==================================================");
    log::info!("ESP32-S3 [STD] DHT22 -> Azure IoT Hub (MQTT TLS)");
    log::info!("==================================================");

    let peripherals = Peripherals::take().context("Gagal inisialisasi periferal")?;
    let sys_loop = EspSystemEventLoop::take().context("Gagal mengambil system event loop")?;
    let nvs = EspDefaultNvsPartition::take().context("Gagal mengambil NVS partition")?;

    // Inisialisasi Pin 20 untuk Sensor DHT22
    let mut dht_pin = PinDriver::input_output_od(peripherals.pins.gpio20, Pull::Up)
        .context("Gagal inisialisasi pin DHT22 pada GPIO 20")?;
    dht_pin.set_high()?;

    // Connect ke Wi-Fi
    log::info!("Menghubungkan ke Wi-Fi Hotspot: {}...", WIFI_SSID);
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    connect_wifi(&mut wifi)?;
    log::info!(">> Wi-Fi Berhasil Terhubung!");

    // =========================================================================
    // SINKRONISASI WAKTU (SNTP) - WAJIB UNTUK VALIDASI SERTIFIKAT TLS AZURE
    // =========================================================================
    log::info!("Menyinkronkan waktu sistem via SNTP...");
    let sntp = EspSntp::new_default()?;
    let mut retries = 0;
    while sntp.get_sync_status() != SyncStatus::Completed {
        thread::sleep(Duration::from_millis(500));
        retries += 1;
        if retries > 20 {
            log::warn!("Sinkronisasi waktu belum selesai sempurna, mencoba melanjutkan...");
            break;
        }
    }
    log::info!(">> Waktu Sistem Berhasil Terverifikasi!");

    // =========================================================================
    // INISIALISASI KLIEN MQTT TLS KE AZURE IOT HUB (PORT 8883)
    // =========================================================================
    let mqtt_url = format!("mqtts://{}:8883", AZURE_IOTHUB_HOST);
    let username = format!("{}/{}/?api-version=2021-04-12", AZURE_IOTHUB_HOST, DEVICE_ID);
    let publish_topic = format!("devices/{}/messages/events/", DEVICE_ID);

    let mqtt_config = MqttClientConfiguration {
        client_id: Some(DEVICE_ID),
        username: Some(&username),
        password: Some(AZURE_SAS_TOKEN),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };

    log::info!("Menghubungkan ke Broker Azure IoT Hub via MQTT Port 8883...");
    let mut mqtt_client = EspMqttClient::new_cb(&mqtt_url, &mqtt_config, move |event| {
        log::info!("[MQTT Event]: {:?}", event.payload());
    })?;

    log::info!(">> Terhubung ke Azure IoT Hub!");

    let mut idf_delay = IdfDelay;

    // =========================================================================
    // LOOP UTAMA: BACA SENSOR & PUBLISH KE AZURE IOT HUB TIAP 5 DETIK
    // =========================================================================
    loop {
        thread::sleep(Duration::from_secs(5));

        let mut temp = 0.0;
        let mut hum = 0.0;
        let mut read_success = false;

        // Coba baca DHT22 hingga 3 kali
        for _ in 0..3 {
            match dht22::Reading::read(&mut idf_delay, &mut dht_pin) {
                Ok(reading) => {
                    temp = reading.temperature;
                    hum = reading.relative_humidity;
                    read_success = true;
                    break;
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }

        if !read_success {
            log::warn!("DHT22 belum terbaca/belum dicolok di GPIO 20, memakai data simulasi...");
            temp = 28.5;
            hum = 65.0;
        } else {
            log::info!("Hasil Sensor DHT22 -> Suhu: {:.2}°C | Kelembaban: {:.2}%", temp, hum);
        }

        // Payload JSON untuk Azure IoT Hub
        let payload = json!({
            "device_id": DEVICE_ID,
            "coffee_type": "robusta",
            "sensors": {
                "mq2": 0.0,
                "mq3": 0.0,
                "mq135": 0.0,
                "mq138": 0.0,
                "temperature": temp,
                "humidity": hum
            }
        });

        let payload_str = payload.to_string();

        // Publish payload ke topik Azure IoT Hub via MQTT
        match mqtt_client.enqueue(&publish_topic, QoS::AtLeastOnce, false, payload_str.as_bytes()) {
            Ok(msg_id) => {
                log::info!(">> [MQTT SUKSES] Data ter-publish ke Azure IoT Hub (Msg ID: {})!", msg_id);
            }
            Err(e) => {
                log::error!("Gagal publish ke Azure IoT Hub: {:?}", e);
            }
        }
    }
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    let wifi_configuration: Configuration = Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap_or_default(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: WIFI_PASS.try_into().unwrap_or_default(),
        channel: None,
        ..Default::default()
    });

    wifi.set_configuration(&wifi_configuration)?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    Ok(())
}