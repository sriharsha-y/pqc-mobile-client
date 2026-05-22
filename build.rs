fn main() {
    uniffi::generate_scaffolding("./src/pqc.udl").expect("UniFFI scaffolding generation failed");
}
