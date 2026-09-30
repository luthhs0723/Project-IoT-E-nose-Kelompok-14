use anyhow::Result;
use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS},
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi, Configuration, ClientConfiguration},
};
use std::{thread::sleep, time::Duration};
use log::*;

// ================== KONFIGURASI WI-FI ==================
const WIFI_SSID: &str = "Galaxy A15 5G";
const WIFI_PASS: &str = "worksomething";

// ================== KONFIGURASI AZURE IOT HUB ==================
// Contoh: "NamaHubAnda.azure-devices.net"
const AZURE_HOST: &str = "mqtts://iothubesp32s3.azure-devices.net:8883"; 
const AZURE_DEVICE_ID: &str = "esp32s3-device-01";

// Format Username Azure IoT: {iothubhostname}/{device_id}/?api-version=2021-04-12
const AZURE_USER: &str = "http://iothubesp32s3.azure-devices.net/esp32s3-device-01/?api-version=2021-04-12";

// SAS Token yang digenerate via Azure CLI / VS Code Extension Azure IoT
// Formatnya diawali dengan "SharedAccessSignature sr=..."
const AZURE_SAS_TOKEN: &str = "SharedAccessSignature sr=iothubesp32s3.azure-devices.net%2Fdevices%2Fesp32s3-device-01&sig=KSx9ZmaccQi5k4rftEi%2BwsbI5WMdO86H4fJla80v0Vs%3D&se=1852030800"; 

fn main() -> Result<()> {
    // Tautkan patch runtime ESP-IDF (Penting untuk kestabilan awal)
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Memulai ESP32 Azure IoT Node...");

    // 1. Inisialisasi Hardware & NVS (Non-Volatile Storage) untuk Wi-Fi
    let peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // 2. Setup Koneksi Wi-Fi secara Blocking
    let mut esp_wifi = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sys_loop)?;

    info!("Menghubungkan ke Wi-Fi: {}", WIFI_SSID);
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASS.try_into().unwrap(),
        ..Default::default()
    }))?;

    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    info!("Wi-Fi Berhasil Terhubung!");

    // 3. Konfigurasi Client MQTT untuk Azure IoT Hub
    // Azure IoT menggunakan MQTTS (Port 8883) secara default, atau Port 443
    let mqtt_config = MqttClientConfiguration {
        client_id: Some(AZURE_DEVICE_ID),
        username: Some(AZURE_USER),
        password: Some(AZURE_SAS_TOKEN),
        server_certificate: None, // Di 'std', defaultnya akan menggunakan sertifikat root bawaan ESP-IDF jika tersedia
        ..Default::default()
    };

    // Alamat URL koneksi MQTT Azure IoT Hub
    let mqtt_url = format!("mqtts://{}", AZURE_HOST);

    info!("Mencoba terhubung ke Azure IoT Hub...");
    let (mut client, mut connection) = EspMqttClient::new_with_conn(&mqtt_url, &mqtt_config)?;

    // Handle background event MQTT di thread terpisah agar koneksi tetap hidup
    std::thread::spawn(move || {
        while let Some(Ok(event)) = connection.next() {
            info!("MQTT Event Received: {:?}", event.payload());
        }
    });

    // Topic standar Azure IoT Hub untuk pengiriman Telemetri (Device-to-Cloud)
    let pub_topic = format!("devices/{}/messages/events/", AZURE_DEVICE_ID);

    // 4. Loop Pengiriman Data Telemetri secara berkala
    let mut count = 0;
    loop {
        count += 1;
        let payload = format!(r#"{{"temperature": 25.5, "humidity": 60, "counter": {}}}"#, count);
        
        info!("Mengirim data ke Azure: {}", payload);
        match client.publish(&pub_topic, QoS::AtLeastOnce, false, payload.as_bytes()) {
            Ok(msg_id) => info!("Data terkirim dengan sukses. Message ID: {:?}", msg_id),
            Err(e) => error!("Gagal mengirim data: {:?}", e),
        }

        // Kirim data setiap 10 detik sekali
        sleep(Duration::from_secs(10));
    }
}