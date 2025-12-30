#![no_std]
#![no_main]

use core::cell::RefCell;
use core::panic::PanicInfo;

use display::{DisplayBuilder, EPD417, EPD417_SIZE};
use embassy_executor::Spawner;
use embassy_net::StackResources;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::Watch;
use esp_hal::gpio::{Level, Pull};
use esp_hal::i2c::master::{Config as I2CConfig, I2c};
use esp_hal::reset::software_reset;
use esp_hal::rng::Trng;
use esp_hal::spi::master::{Config as SPIConfig, Spi};
use esp_hal::time::RateExtU32;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{
    Async,
    gpio::{Input, Output},
    timer::systimer::SystemTimer,
};
use esp_hal_embassy::main;

use esp_wifi::EspWifiController;
use esp_wifi::wifi::WifiStaDevice;
use main::NUM_NOTIFICATION_RECEIVERS;
use main::spi::SpiV2;
use main::tasks::task_display_handler::{
    DISPLAY_CHANNEL_SIZE, DisplayMessage, task_display_handler,
};
use main::tasks::task_nfc::task_nfc;
use main::tasks::task_run::task_run;
use main::tasks::task_wifi_runner::task_wifi_runner;
use main::{NotificationType, initalize_logger, initialize_peripherals, mk_static};
use nfc::{Nfc, STM25DV64KC};
#[panic_handler]
fn panic_handler(info: &PanicInfo) -> ! {
    // Print panic info
    log::info!("Panic occurred!");
    if let Some(location) = info.location() {
        log::info!(
            "  at {}:{}:{}",
            location.file(),
            location.line(),
            location.column()
        );
    }

    // Get and print backtrace
    log::info!("Backtrace:");
    let backtrace = esp_backtrace::arch::backtrace();
    for addr in backtrace.iter().flatten() {
        log::info!("  0x{:x}", addr);
    }

    // Now reset the chip
    software_reset();
    loop {}
}

// https://github.com/search?q=esp_wifi%3A%3A&type=code
#[main]
async fn main(spawner: Spawner) {
    initalize_logger();
    let peripherals = initialize_peripherals();

    esp_alloc::heap_allocator!(130 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sys_timer = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(sys_timer.alarm0);
    let rng = Trng::new(peripherals.RNG, peripherals.ADC1);
    let rng_ref = &*mk_static!(RefCell<Trng>, RefCell::new(rng));

    let init_wifi = &*mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(
            timg0.timer0,
            // This rng is not used. If it ever is in the future then re-write to code
            // so concurrent access does not cause run-time errors
            rng_ref.borrow().rng,
            peripherals.RADIO_CLK,
        )
        .unwrap()
    );

    let (wifi_interface, wifi_controller) =
        esp_wifi::wifi::new_with_mode(&init_wifi, peripherals.WIFI, WifiStaDevice).unwrap();

    let seed_msb = (rng_ref.borrow_mut().random() as u64) << 32;
    let wifi_seed = seed_msb | rng_ref.borrow_mut().random() as u64;

    let (stack, runner) = embassy_net::new(
        wifi_interface,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        wifi_seed,
    );

    let bs = Input::new(peripherals.GPIO20, Pull::Down); // Busy is low. So default should be low
    let cs = Output::new(peripherals.GPIO21, Level::High);
    let dc = Output::new(peripherals.GPIO5, Level::High);
    let rst = Output::new(peripherals.GPIO4, Level::High);

    let spi = Spi::new(
        peripherals.SPI2,
        SPIConfig::default().with_frequency(30u32.MHz()),
    )
    .unwrap()
    .with_mosi(peripherals.GPIO10)
    .with_sck(peripherals.GPIO8)
    .into_async();

    let indicator = Output::new(peripherals.GPIO2, Level::Low);
    let display = mk_static!(
        DisplayBuilder<SpiV2<'static, Async>, Input<'static>, Output<'static>, EPD417_SIZE, EPD417>,
        DisplayBuilder::new(SpiV2::from(spi), cs, rst, dc, bs)
    )
    .with_max_cycles(30)
    .build();

    // Create display channel for rate-limited display updates
    let display_channel = mk_static!(
        Channel<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>,
        Channel::<NoopRawMutex, DisplayMessage, DISPLAY_CHANNEL_SIZE>::new()
    );

    let gpo = Input::new(peripherals.GPIO9, Pull::Up);
    let vcc_nfc = Output::new(peripherals.GPIO3, Level::Low);
    let i2c = I2c::new(peripherals.I2C0, I2CConfig::default())
        .unwrap()
        .with_scl(peripherals.GPIO6)
        .with_sda(peripherals.GPIO7)
        .into_async();

    let nfc = Nfc::<STM25DV64KC, _, _, _>::new(i2c, gpo, vcc_nfc);
    let notif = mk_static!(
        Watch<NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>,
        Watch::<NoopRawMutex, NotificationType, NUM_NOTIFICATION_RECEIVERS>::new()
    );
    let sender = notif.sender();
    let receiver = notif.receiver().unwrap();

    spawner.must_spawn(task_nfc(nfc, sender));
    spawner.must_spawn(task_wifi_runner(runner));
    spawner.must_spawn(task_display_handler(display, display_channel, indicator));
    spawner.must_spawn(task_run(
        stack,
        rng_ref,
        wifi_controller,
        receiver,
        display_channel,
        peripherals.SHA,
        peripherals.RSA,
    ));
}
