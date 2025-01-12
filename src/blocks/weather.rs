use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;
use crossbeam_channel::Sender;
use serde_json;
use uuid::Uuid;

use crate::block::{Block, ConfigBlock};
use crate::config::Config;
use crate::de::deserialize_duration;
use crate::errors::*;
use crate::input::{I3BarEvent, MouseButton};
use crate::scheduler::Task;
use crate::util::FormatTemplate;
use crate::widgets::button::ButtonWidget;
use crate::widget::I3BarWidget;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "name", rename_all = "lowercase")]
pub enum WeatherService {
    // TODO:
    // DarkSky {
    //     token: String,
    //     latitude: f64,
    //     longitude: f64,
    //     units: Option<InputUnit>
    // },
    OpenWeatherMap {
        api_key: String,
        city_id: String,
        units: OpenWeatherMapUnits,
    },
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenWeatherMapUnits {
    Metric,
    Imperial,
}

pub struct Weather {
    id: String,
    weather: ButtonWidget,
    format: String,
    weather_keys: HashMap<String, String>,
    service: WeatherService,
    update_interval: Duration,
}

impl Weather {
    fn update_weather(&mut self) -> Result<()> {
        match self.service {
            WeatherService::OpenWeatherMap {
                ref api_key,
                ref city_id,
                ref units,
            } => {
                let output = Command::new("sh")
                    .args(
                        &[
                            "-c",
                            &format!(
                                "curl -m 3 \"http://api.openweathermap.org/data/2.5/weather?id={city_id}&appid={api_key}&units={units}\" 2> /dev/null",
                                city_id = city_id,
                                api_key = api_key,
                                units = match *units {
                                    OpenWeatherMapUnits::Metric => "metric",
                                    OpenWeatherMapUnits::Imperial => "imperial",
                                },
                            ),
                        ],
                    )
                    .output()
                    .block_error("weather", "Failed to exectute curl.")
                    .and_then(|raw_output| {
                        String::from_utf8(raw_output.stdout).block_error("weather", "Non-UTF8 SSID.")
                    })?;

                // Don't error out on empty responses e.g. for when not
                // connected to the internet.
                if output.is_empty() {
                    self.weather.set_icon("weather_default");
                    self.weather_keys = HashMap::new();
                    return Ok(());
                }

                let json: serde_json::value::Value = serde_json::from_str(&output).block_error(
                    "weather",
                    "Failed to parse JSON response.",
                )?;

                // Try to convert an API error into a block error.
                if let Some(val) = json.get("message") {
                    return Err(BlockError(
                        "weather".to_string(),
                        format!("API Error: {}", val.as_str().unwrap()),
                    ));
                };
                let raw_weather = match json.pointer("/weather/0/main")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()) {
                    Some(v) => v,
                    None => {
                        return Err(BlockError(
                            "weather".to_string(),
                            "Malformed JSON.".to_string(),
                        ));
                    }
                };
                let raw_temp = match json.pointer("/main/temp").and_then(|v| v.as_f64()) {
                    Some(v) => v,
                    None => {
                        return Err(BlockError(
                            "weather".to_string(),
                            "Malformed JSON.".to_string(),
                        ));
                    }
                };
                let raw_wind_speed = match json.pointer("/wind/speed").and_then(|v| v.as_f64()) {
                    Some(v) => v,
                    None => {
                        return Err(BlockError(
                            "weather".to_string(),
                            "Malformed JSON.".to_string(),
                        ));
                    }
                };
                let raw_wind_direction = match json.pointer("/wind/deg").and_then(|v| v.as_f64()) {
                    Some(v) => v,
                    None => {
                        return Err(BlockError(
                            "weather".to_string(),
                            "Malformed JSON.".to_string(),
                        ));
                    }
                };
                let raw_location = match json.pointer("/name").and_then(|v| v.as_str()).map(|s| {
                    s.to_string()
                }) {
                    Some(v) => v,
                    None => {
                        return Err(BlockError(
                            "weather".to_string(),
                            "Malformed JSON.".to_string(),
                        ));
                    }
                };

                // Convert wind direction in azimuth degrees to abbreviation names
                fn convert_wind_direction(direction: f64) -> String {
                    match direction.round() as i64 {
                        24 ... 68 => "NE".to_string(),
                        69 ... 113 => "E".to_string(),
                        114 ... 158 => "SE".to_string(),
                        159 ... 203 => "S".to_string(),
                        204 ... 248 => "SW".to_string(),
                        249 ... 293 => "W".to_string(),
                        294 ... 338 => "NW".to_string(),
                        _ => "N".to_string()
                    }
                }

                self.weather.set_icon(match raw_weather.as_str() {
                    "Clear" => "weather_sun",
                    "Rain" | "Drizzle" => "weather_rain",
                    "Clouds" | "Fog" | "Mist" => "weather_clouds",
                    "Thunderstorm" => "weather_thunder",
                    "Snow" => "weather_snow",
                    _ => "weather_default",
                });

                self.weather_keys =
                    map_to_owned!("{weather}" => raw_weather,
                                  "{temp}" => format!("{:.0}", raw_temp),
                                  "{wind}" => format!("{:.1}", raw_wind_speed),
                                  "{direction}" => convert_wind_direction(raw_wind_direction),
                                  "{location}" => raw_location);
                Ok(())
            }
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    #[serde(default = "WeatherConfig::default_interval", deserialize_with = "deserialize_duration")]
    pub interval: Duration,
    #[serde(default = "WeatherConfig::default_format")]
    pub format: String,
    pub service: WeatherService,
}

impl WeatherConfig {
    fn default_interval() -> Duration {
        Duration::from_secs(600)
    }

    fn default_format() -> String {
        "{weather} {temp}\u{00b0}".to_string()
    }
}

impl ConfigBlock for Weather {
    type Config = WeatherConfig;

    fn new(block_config: Self::Config, config: Config, _tx_update_request: Sender<Task>) -> Result<Self> {
        let id = Uuid::new_v4().simple().to_string();
        Ok(Weather {
            id: id.clone(),
            weather: ButtonWidget::new(config, &id),
            format: block_config.format,
            weather_keys: HashMap::new(),
            service: block_config.service,
            update_interval: block_config.interval,
        })
    }
}

impl Block for Weather {
    fn update(&mut self) -> Result<Option<Duration>> {
        self.update_weather()?;
        // Display an error/disabled-looking widget when we don't have any
        // weather information, which is likely due to internet connectivity.
        if self.weather_keys.keys().len() == 0 {
            self.weather.set_text("×".to_string());
        } else {
            let fmt = FormatTemplate::from_string(&self.format)?;
            self.weather.set_text(fmt.render(&self.weather_keys));
        }
        Ok(Some(self.update_interval))
    }

    fn view(&self) -> Vec<&I3BarWidget> {
        vec![&self.weather]
    }

    fn click(&mut self, event: &I3BarEvent) -> Result<()> {
        if event.matches_name(self.id()) {
            if let MouseButton::Left = event.button {
                    self.update()?;
            }
        }
        Ok(())
    }

    fn id(&self) -> &str {
        &self.id
    }
}
