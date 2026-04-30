fn main() {
    println!("env XDG_DATA_HOME = {:?}", std::env::var("XDG_DATA_HOME"));
    let dirs = directories::ProjectDirs::from("", "", "task-journal").unwrap();
    println!("data_local_dir() = {:?}", dirs.data_local_dir());
    println!("data_dir() = {:?}", dirs.data_dir());
}
