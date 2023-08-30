// mod enclave_interface;

use sgxlib_enclave::unsafe_ecall;
use sgxlib_enclave::Enclave;

use sgxlib_enclave::sgx_types;
include!(concat!(env!("OUT_DIR"), "/ecall.rs"));

use serde_json;
use std::ffi::CString;

fn main() {
    glog::init();

    let enclave = Enclave::new("automata_mempool_enclave");

    let args: Vec<String> = std::env::args().collect();
    let args = serde_json::to_string(&args).unwrap();
    let args_ptr = CString::new(args.as_str()).unwrap();
    apps::set_ctrlc({
        let enclave = enclave.clone();
        move || {
            unsafe_ecall!(enclave.eid(), enclave_terminate()).unwrap();
        }
    });
    unsafe_ecall!(
        enclave.eid(),
        enclave_entrypoint(enclave.eid(), args_ptr.as_ptr())
    )
    .unwrap();
    glog::info!("trusted: {:?}", enclave.eid());
}

#[no_mangle]
pub unsafe extern "C" fn ra_get_epid_group_id() -> u32 {
    sgxlib_ra::RaFfi::get_epid_gpid()
}
