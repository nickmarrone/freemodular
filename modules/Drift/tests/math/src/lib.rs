#[path = "../../../Firmware/src"]
pub mod firmware {
    pub mod bezier;
    pub mod brownian;
    pub mod lfo;
    pub mod perlin;
    pub mod random;
    pub mod shared;
}
pub use firmware::*;

#[cfg(test)]
mod tests;
