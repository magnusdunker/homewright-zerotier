fn main() {
    thehome_plugin::run(thehome_zerotier::ZeroTier::default());
    // libzt's threads must stop before the process tears down its statics
    thehome_zerotier::zt::stop();
    std::process::exit(0);
}
