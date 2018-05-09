//! A user friendly crate that allows you to share memory between __processes__
//!
//! ## Examples
//! Creator based on examples/create.rs
//! ```
//! //Create a SharedMem at `pwd`\shared_mem.link that links to a shared memory mapping of size 4096 and managed by a mutex.
//! let mut my_shmem: SharedMem = match SharedMem::create(PathBuf::from("shared_mem.link") LockType::Mutex, 4096).unwrap();
//! //Set explicit scope for the lock (no need to call drop(shared_data))
//! {
//!     //Acquire write lock
//!     let mut shared_data = match my_shmem.wlock_as_slice::<u8>().unwrap();
//!     let src = b"Some string you want to share\x00";
//!     //Write to the shared memory
//!     shared_data[0..src.len()].copy_from_slice(src);
//! }
//! ```
//!
//! Slave based on examples/open.rs
//! ```
// Open an existing SharedMem from `pwd`\shared_mem.link
//! let mut my_shmem: SharedMem = match SharedMem::open(PathBuf::from("shared_mem.link")).unwrap();
//! //Set explicit scope for the lock (no need to call drop(shared_data))
//! {
//!     //Acquire read lock
//!     let mut shared_data = match my_shmem.rlock_as_slice::<u8>().unwrap();
//!     //Print the content of the shared memory as chars
//!     for byte in &shared_data[0..256] {
//!         if *byte == 0 { break; }
//!         print!("{}", *byte as char);
//!     }
//! }
//! ```

#[macro_use]
extern crate cfg_if;

//Load up the proper OS implementation
cfg_if! {
    if #[cfg(target_os="windows")] {
        mod win;
        use win as os_impl;
    } else if #[cfg(any(target_os="freebsd", target_os="linux", target_os="macos"))] {
        mod nix;
        use nix as os_impl;
    } else {
        compile_error!("This library isnt implemented for this platform...");
    }
}

//Include definitions from locking.rs
mod locking;
pub use locking::*;

use std::path::PathBuf;
use std::fs::{File};
use std::io::{Write, Read};
use std::fs::remove_file;
use std::slice;
use std::os::raw::c_void;
use std::ptr::null_mut;
use std::mem::size_of;

type Result<T> = std::result::Result<T, Box<std::error::Error>>;

struct MetaDataHeader {
    meta_size: usize,
    user_size: usize,
    num_locks: usize,
    num_events: usize,
}
struct LockHeader {
    uid: u8,
    offset: usize,
    length: usize,
}
struct EventHeader {
    event_id: u8,
}

//Holds information about the mapping
pub struct SharedMemConf<'a> {
    owner: bool,
    link_path: PathBuf,
    size: usize,

    meta_size: usize,
    lock_data: Vec<GenericLock<'a>>,
    event_data: Vec<GenericEvent>,
}
impl<'a> SharedMemConf<'a> {

    pub fn valid_lock_range(map_size: usize, offset: usize, length:usize) -> bool {

        if offset == map_size {
            return false;
        } else if offset + length > map_size {
            return false;
        }

        return true;
    }

    //Returns an initialized SharedMemConf
    pub fn new(new_link_path: PathBuf, map_size: usize) -> SharedMemConf<'a> {
        SharedMemConf {
            owner: false,
            link_path: new_link_path,
            size: map_size,
            //read_only: false,
            lock_data: Vec::with_capacity(2),
            event_data: Vec::with_capacity(2),
            meta_size: size_of::<MetaDataHeader>(),
        }
    }

    //Adds a lock of specified type on the specified byte indexes to the config
    pub fn add_lock(mut self, lock_type: LockType, offset: usize, length: usize) -> Result<SharedMemConf<'a>> {

        if !SharedMemConf::valid_lock_range(self.size, offset, length) {
            return Err(From::from("Invalid lock range"));
        }

        //TODO : Validate that this lock doesnt overlap data covered by another lock ?

        let new_lock = GenericLock {
            uid: (lock_type as u8),
            offset: offset,
            length: length,
            lock_ptr: null_mut(),
            data_ptr: null_mut(),
            interface: os_impl::lockimpl_from_type(&lock_type),
        };

        //Add the size of this lock to our metadata size
        self.meta_size += size_of::<LockHeader>() + new_lock.interface.size_of();

        //Add this lock to our config
        self.lock_data.push(new_lock);

        Ok(self)
    }
    pub fn get_user_size(&self) -> &usize {
        return &self.size;
    }
    pub fn get_metadata_size(&self) -> &usize {
        return &self.meta_size;
    }

    //Creates a shared memory mapping from the config
    pub fn create(mut self) -> Result<SharedMem<'a>> {

        //Create link file asap
        let mut cur_link: File;
        if self.link_path.is_file() {
            return Err(From::from("Cannot create SharedMem because file already exists"));
        } else {
            cur_link = File::create(&self.link_path)?;
            self.owner = true;
        }

        let some_str: String = String::from("test_mapping");

        //Create the file mapping
        let os_map: os_impl::MapData = os_impl::create_mapping(&some_str, self.meta_size + self.size)?;

        let mut cur_ptr = os_map.map_ptr as usize;
        let user_ptr = os_map.map_ptr as usize + self.meta_size;

        //Initialize meta data
        let meta_header: &mut MetaDataHeader = unsafe{&mut (*(cur_ptr as *mut MetaDataHeader))};
        meta_header.meta_size = self.meta_size;
        meta_header.user_size = self.size;
        meta_header.num_locks = self.lock_data.len();
        meta_header.num_events = self.event_data.len();
        cur_ptr += size_of::<MetaDataHeader>();

        //Initialize locks
        for lock in &mut self.lock_data {
            //Set lock header
            let lock_header: &mut LockHeader = unsafe{&mut (*(cur_ptr as *mut LockHeader))};
            lock_header.uid = lock.uid;
            lock_header.offset = lock.offset;
            lock_header.length = lock.length;
            cur_ptr += size_of::<LockHeader>();
            //Set lock pointer
            lock.lock_ptr = cur_ptr as *mut c_void;
            lock.data_ptr = (user_ptr + lock.offset) as *mut c_void;
            cur_ptr += lock.interface.size_of();

            //Initialize the lock
            lock.interface.init(lock, true)?;
        }

        //Initialize events
        for event in &mut self.event_data {
            //Set lock header
            let event_header: &mut EventHeader = unsafe{&mut (*(cur_ptr as *mut EventHeader))};
            event_header.event_id = event.uid;
            cur_ptr += size_of::<EventHeader>();
            //Set lock pointer
            event.ptr = cur_ptr as *mut c_void;

            //Initialize the event
            //cur_ptr += event.interface.size_of();
            //TODO : event.interface.init(event)?;
        }

        match cur_link.write(some_str.as_bytes()) {
            Ok(write_sz) => if write_sz != some_str.as_bytes().len() {
                return Err(From::from("Failed to write full contents info on disk"));
            },
            Err(_) => return Err(From::from("Failed to write info on disk")),
        };

        println!("Created map with:
        MetaSize : {}
        Size : {}
        Num locks : {}
        Num Events : {}
        MetaAddr {:p}
        UserAddr 0x{:x}",
            meta_header.meta_size,
            meta_header.user_size,
            meta_header.num_locks,
            meta_header.num_events,
            os_map.map_ptr,
            user_ptr,
        );

        Ok(SharedMem {
            conf: self,
            os_data: os_map,
            link_file: cur_link,
        })
    }
}

///Struct used to manipulate the shared memory
pub struct SharedMem<'a> {
    //Config that describes this mapping
    conf: SharedMemConf<'a>,
    //The currently in use link file
    link_file: File,
    //Os specific data for the mapping
    os_data: os_impl::MapData,
}
impl<'a> Drop for SharedMem<'a> {

    ///Deletes the SharedMemConf artifacts
    fn drop(&mut self) {

        //Close the openned link file
        drop(&self.link_file);

        //Delete link file if we own it
        if self.conf.owner {
            if self.conf.link_path.is_file() {
                match remove_file(&self.conf.link_path) {_=>{},};
            }
        }
    }
}

impl<'a> SharedMem<'a> {

    pub fn create(new_link_path: PathBuf, lock_type: LockType, size: usize) -> Result<SharedMem<'a>> {
        //Create a simple sharedmemconf with one lock
        SharedMemConf::new(new_link_path, size).add_lock(lock_type, 0, size).unwrap().create()
    }
    pub fn open(existing_link_path: PathBuf) -> Result<SharedMem<'a>> {

        // Make sure the link file exists
        if !existing_link_path.is_file() {
            return Err(From::from("Cannot open SharedMem, link file doesnt exists"));
        }

        //Get real_path from link file
        let mut cur_link = File::open(&existing_link_path)?;
        let mut file_contents: Vec<u8> = Vec::with_capacity(existing_link_path.to_string_lossy().len() + 5);
        cur_link.read_to_end(&mut file_contents)?;
        let real_path: String = String::from_utf8(file_contents)?;

        //Attempt to open the mapping
        let os_map = os_impl::open_mapping(&real_path)?;

        if size_of::<MetaDataHeader>() > os_map.map_size {
            return Err(From::from("Mapping is smaller than our metadata header size !"));
        }

        //Initialize meta data
        let mut cur_ptr = os_map.map_ptr as usize;

        //Read header for basic info
        let meta_header: &mut MetaDataHeader = unsafe{&mut (*(cur_ptr as *mut MetaDataHeader))};
        cur_ptr += size_of::<MetaDataHeader>();

        let mut map_conf: SharedMemConf = SharedMemConf {
            owner: false,
            link_path: existing_link_path,
            size: meta_header.user_size,
            //read_only: false,
            lock_data: Vec::with_capacity(meta_header.num_locks),
            event_data: Vec::with_capacity(meta_header.num_events),
            meta_size: meta_header.meta_size,
        };

        //Basic size check on (metadata size + userdata size)
        if os_map.map_size < (map_conf.meta_size + map_conf.size) {
            return Err(From::from(
                format!("Shared memory header contains an invalid mapping size : (map_size: {}, meta_size: {}, user_size: {})",
                    os_map.map_size,
                    map_conf.size,
                    map_conf.meta_size)
            ));
        }

        //Add the metadata size to our base pointer to get user addr
        let user_ptr = os_map.map_ptr as usize + map_conf.meta_size;

        println!("Openned map with:
        MetaSize : {}
        Size : {}
        Num locks : {}
        Num Events : {}
        MetaAddr {:p}
        UserAddr 0x{:x}",
            meta_header.meta_size,
            meta_header.user_size,
            meta_header.num_locks,
            meta_header.num_events,
            os_map.map_ptr,
            user_ptr,
        );

        for i in 0..meta_header.num_locks {

            let lock_header: &mut LockHeader = unsafe{&mut (*(cur_ptr as *mut LockHeader))};
            cur_ptr += size_of::<LockHeader>();
            //Make sure address is valid before reading lock header
            if cur_ptr > user_ptr {
                return Err(From::from("Shared memory metadata is invalid... Not enought space to read lock header fields"));
            }

            //Try to figure out the lock type from the given ID
            let lock_type: LockType = lock_uid_to_type(&lock_header.uid)?;

            println!("\tFound new lock \"{:?}\" : offset {} length {}", lock_type, lock_header.offset, lock_header.length);

            //Make sure the lock range makes sense
            if !SharedMemConf::valid_lock_range(map_conf.size, lock_header.offset, lock_header.length) {
                return Err(From::from("Invalid lock range"));
            }

            let mut new_lock = GenericLock {
                uid: lock_type as u8,
                lock_ptr: cur_ptr as *mut c_void,
                data_ptr: (user_ptr + lock_header.offset) as *mut c_void,
                offset: lock_header.offset,
                length: lock_header.length,
                interface: os_impl::lockimpl_from_type(&lock_type),
            };
            cur_ptr += new_lock.interface.size_of();

            //Make sure memory is big enough to hold lock data
            if cur_ptr > user_ptr {
                return Err(From::from(
                    format!("Shared memory metadata is invalid... Trying to read lock {} of size 0x{:x} at address 0x{:x} but user data starts at 0x{:x}..."
                        , i, new_lock.interface.size_of(), cur_ptr, user_ptr)
                ));
            }

            //Allow the lock to init itself
            new_lock.interface.init(&mut new_lock, false)?;

            //Save this lock in our conf
            map_conf.lock_data.push(new_lock);
        }

        for _i in 0..meta_header.num_events {
            if cur_ptr >= user_ptr {
                return Err(From::from("Shared memory metadata is invalid... Not enough space for events"));
            }
            let _event_header: &mut EventHeader = unsafe{&mut (*(cur_ptr as *mut EventHeader))};
            cur_ptr += size_of::<EventHeader>();

            //TODO : Init events here
        }

        if cur_ptr != user_ptr {
            return Err(From::from(format!("Shared memory metadata does not end right before user data ! 0x{:x} != 0x{:x}", cur_ptr, user_ptr)));
        }

        Ok(SharedMem {
            conf: map_conf,
            os_data: os_map,
            link_file: cur_link,
        })
    }

    ///Returns the size of the SharedMem
    pub fn get_size(&self) -> &usize {
        &self.conf.size
    }
    ///Returns the link_path of the SharedMem
    pub fn get_link_path(&self) -> &PathBuf {
        &self.conf.link_path
    }
    ///Returns the OS specific path of the shared memory object
    ///
    /// Usualy on Linux, this will point to a file under /dev/shm/
    ///
    /// On Windows, this returns a namespace
    pub fn get_real_path(&self) -> &String {
        &self.os_data.unique_id
    }
}

pub struct SharedMemRaw<'a> {
    //Config that describes this mapping
    conf: SharedMemConf<'a>,
    //Os specific data for the mapping
    os_data: os_impl::MapData,
}

impl<'a> SharedMemRaw<'a> {

    pub fn create(new_link_path: PathBuf, lock_type: LockType, size: usize) -> Result<SharedMem<'a>> {
        //Create a simple sharedmemconf with one lock
        SharedMemConf::new(new_link_path, size).add_lock(lock_type, 0, size).unwrap().create()
    }
    pub fn open(unique_id: String) -> Result<SharedMemRaw<'a>> {

        //Attempt to open the mapping
        let os_map = os_impl::open_mapping(&unique_id)?;

        let map_conf: SharedMemConf = SharedMemConf {
            owner: false,
            link_path: PathBuf::new(), //No link path for raw shared memory
            size: os_map.map_size,
            lock_data: Vec::new(),
            event_data: Vec::new(),
            meta_size: 0,
        };

        Ok(SharedMemRaw {
            conf: map_conf,
            os_data: os_map,
        })
    }

    ///Returns the size of the SharedMem
    pub fn get_size(&self) -> &usize {
        &self.conf.size
    }
    ///Returns the OS specific path of the shared memory object
    ///
    /// Usualy on Linux, this will point to a file under /dev/shm/
    ///
    /// On Windows, this returns a namespace
    pub fn get_path(&self) -> &String {
        &self.os_data.unique_id
    }
}

/// Read [WARNING](trait.SharedMemCast.html#warning) before use
///
/// Trait used to indicate that a type can be cast over the shared memory.
///
/// For now, shared_memory implements the trait on almost all primitive types.
///
/// ### __<span style="color:red">WARNING</span>__
///
/// Only implement this trait if you understand the implications of mapping Rust types to shared memory.
/// When doing so, you should be mindful of :
/// * Does my type have any pointers in its internal representation ?
///    * This is important because pointers in your type need to also point to the shared memory for it to be usable by other processes
/// * Can my type resize its contents ?
///    * If so, the type probably cannot be safely used over shared memory because your type might call alloc/realloc/free on shared memory addresses
/// * Does my type allow for initialisation after instantiation ?
///    * A [R|W]lock to the shared memory returns a reference to your type. That means that any use of that reference assumes that the type was properly initialized.
///
/// An example of a type that __shouldnt__ be cast to the shared memory would be Vec.
/// Vec internaly contains a pointer to a slice containing its data and some other metadata.
/// This means that to cast a Vec to the shared memory, the memory has to already be initialized with valid pointers and metadata.
/// Granted we could initialize those fields manually, the use of the vector might then trigger a free/realloc on our shared memory.
///
/// # Examples
/// ```
/// struct SharedState {
///     num_listenners: u32,
///     message: [u8; 256],
/// }
/// //WARNING : Only do this if you know what you're doing.
/// unsafe impl SharedMemCast for SharedState {}
///
/// <...>
///
/// {
///     let mut shared_state: WriteLockGuard<SharedState> = match my_shmem.wlock().unwrap();
///     shared_state.num_listenners = 0;
///     let src = b"Welcome, we currently have 0 listenners !\x00";
///     shared_state.message[0..src.len()].copy_from_slice(src);
/// }
///```
pub unsafe trait SharedMemCast {}
unsafe impl SharedMemCast for bool {}
unsafe impl SharedMemCast for char {}
unsafe impl SharedMemCast for str {}
unsafe impl SharedMemCast for i8 {}
unsafe impl SharedMemCast for i16 {}
unsafe impl SharedMemCast for i32 {}
unsafe impl SharedMemCast for u8 {}
unsafe impl SharedMemCast for i64 {}
unsafe impl SharedMemCast for u16 {}
unsafe impl SharedMemCast for u64 {}
unsafe impl SharedMemCast for isize {}
unsafe impl SharedMemCast for u32 {}
unsafe impl SharedMemCast for usize {}
unsafe impl SharedMemCast for f32 {}
unsafe impl SharedMemCast for f64 {}
