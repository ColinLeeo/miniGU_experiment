use std::cell::Cell;
#[derive(Clone)]
pub struct Volatile<T: Copy> {
    data: Cell<T>,
}

impl<T: Copy> Volatile<T> {
    pub fn new(data: T) -> Self {
        let data = Cell::new(data);
        Volatile { data }
    }

    pub fn get(&self) -> T {
        self.data.get()
    }

    pub fn set(&self, data: T) {
        self.data.set(data)
    }
}

unsafe impl<T: Copy> Send for Volatile<T> {}

unsafe impl<T: Copy> Sync for Volatile<T> {}
