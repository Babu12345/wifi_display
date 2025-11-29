//! SPI implementation for esp_hal

use esp_hal::{DriverMode, spi::master::Spi};
use spi::TSpi;

/// SpiV2 which implements the hardware agnostic TSpi
pub struct SpiV2<'a, M>(Spi<'a, M>);

impl<'a, M> From<Spi<'a, M>> for SpiV2<'a, M> {
    fn from(value: Spi<'a, M>) -> Self {
        Self(value)
    }
}

impl<'d, M> TSpi for SpiV2<'d, M>
where
    M: DriverMode,
{
    fn transfer_and_receive_bytes<'a>(&mut self, words: &'a mut [u8]) -> error::Result<&'a [u8]> {
        let result = self
            .0
            .transfer(words)
            .map_err(|_| error::Error::SPIFailedTransfer)?;
        Ok(result)
    }

    fn transfer_bytes<'a>(&mut self, words: &'a mut [u8]) -> error::Result<()> {
        self.0
            .transfer(words)
            .map_err(|_| error::Error::SPIFailedTransfer)?;
        Ok(())
    }
}

#[cfg(test)]
#[embedded_test::tests]
mod tests {}
