use byte_unit::Byte;
use bytes::Bytes;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use scraper::{Html, Selector};
use std::cmp::min;
use std::convert::TryInto;
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io;
use tokio::sync::mpsc;

pub mod arg;

const H1: &str = "h1#title";
const TABLE: &str = "#table :nth-child(1) > span > a[href]";
const SIZE: &str = "body > section > div > nav > div:nth-child(2) > div > p.title";
const BATCH_SIZE: usize = 3; // How many URLs we want to process each step

#[derive(Debug)]
enum Image {
    Image {
        name: String,
        path: String,
        size: u128,
        data: Bytes,
    },
}

pub async fn download_albums(mut albums: Vec<String>) -> Result<(), Box<dyn Error>> {
    let m = Arc::new(MultiProgress::new());
    let m2 = m.clone();
    let sty = ProgressStyle::default_bar()
        .template("{prefix:>12.cyan.bold} [{bar:.green} {msg}{pos}/{len}]")
        .progress_chars("█▒░");
    let pb_main = Arc::new(m.add(ProgressBar::new(albums.len().try_into().unwrap())));
    pb_main.set_style(sty.clone());
    pb_main.set_prefix("Overall");
    pb_main.tick();
    let multibar = {
        let multibar = m2.clone();
        tokio::task::spawn_blocking(move || multibar.join())
    };

    let mut idx: usize = 0;
    // Download few albums per run
    while albums.len() != 0 {
        let mut handles = Vec::new();
        let mut queue: Vec<String>;
        if BATCH_SIZE < albums.len() {
            queue = albums.drain(..BATCH_SIZE).collect();
        } else {
            queue = albums.drain(..).collect();
        }
        for q in queue {
            let mb = m.clone();
            let pb = pb_main.clone();
            handles.push(tokio::spawn(async move {
                let _ = download_album(pb, mb, q.to_string()).await;
            }));
        }
        //futures::future::join_all(handles).await;
        for h in handles {
            h.await.unwrap();
        }
        idx += BATCH_SIZE;
    }
    pb_main.finish();
    multibar.await??;
    Ok(())
}

pub async fn download_album(
    pb_main: Arc<ProgressBar>,
    mb: Arc<MultiProgress>,
    url: String,
) -> Result<(), Box<dyn Error>> {
    let (title, images, size) = crawl_album(&url).await?;
    let dir = format!("./cyberdrop-dl/{}", title);
    let size = Byte::from_str(size).unwrap();
    create_dir(&dir).await;
    let mut downloaded: u128 = 0;
    let total_size: u128 = size.get_bytes().try_into().unwrap();
    let pb = mb.add(ProgressBar::new(total_size.try_into().unwrap()));
    pb.set_style(ProgressStyle::default_bar().template("{prefix:>10.cyan.bold} {spinner:.green} [{bar:.green} {msg}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) ").progress_chars("█▒░"));
    pb.set_prefix(format!("{}", title));
    let client = reqwest::Client::builder().build()?;

    let (tx, mut rx) = mpsc::channel(100);

    let worker = tokio::spawn(async move {
        for i in images {
            let (name, path, size, data) = download_image2(&dir, &i, &client).await.unwrap();
            let img = Image::Image {
                name: name.to_string(),
                path: path,
                size: size,
                data: data,
            };
            tx.send(img).await.unwrap();
        }
    });
    /*
        tokio::spawn(async move {
            tx.send("sending from first handle").await.unwrap();
        });
    */
    //tokio::join!(worker);
    let pb2 = pb.clone();
    let manager = tokio::spawn(async move {
        while let Some(img) = rx.recv().await {
            match img {
                Image::Image {
                    name,
                    path,
                    size,
                    data,
                } => {
                    //println!("{} {} {}", name, path, size);
                    let fname = name;
                    let mut dest = path;
                    dest.push_str(&fname);
                    let mut reader: &[u8] = &data;
                    let mut file = File::create(&dest).await.unwrap();
                    let bytes = io::copy(&mut reader, &mut file).await.unwrap();
                    //let bytes = Byte::from_bytes(bytes.into());
                    let new = min(downloaded + size, total_size);
                    downloaded = new;
                    pb2.set_position(new.try_into().unwrap());
                }
            }
            //        println!("GOT = {}", img);
            //let fname = image_name_from_url(url).await?;
        }
    });
    worker.await.unwrap();
    manager.await.unwrap();

    pb.finish_with_message("✔️");
    pb_main.inc(1);
    /*
        let (title, images, size) = crawl_album(url).await?;
        let dir = format!("./cyberdrop-dl/{}", title);
        let size = Byte::from_str(size).unwrap();
        create_dir(&dir).await;
        let mut downloaded: u128 = 0;
        let total_size: u128 = size.get_bytes().try_into().unwrap();
        let pb = mb.add(ProgressBar::new(total_size.try_into().unwrap()));
        pb.set_style(ProgressStyle::default_bar().template("{prefix:>10.cyan.bold} {spinner:.green} [{bar:.green} {msg}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) ").progress_chars("█▒░"));
        pb.set_prefix(format!("{}", title));
        let client = reqwest::Client::builder().build()?;
        for i in images {
            let bytes = download_image(&dir, &i, &client).await?;
            let new = min(downloaded + bytes, total_size);
            downloaded = new;
            pb.set_position(new.try_into().unwrap());
        }
        pb.finish_with_message("✔️");
        pb_main.inc(1);
    */
    Ok(())
}

pub async fn crawl_album(url: &String) -> Result<(String, Vec<String>, String), Box<dyn Error>> {
    //println!("Trying to extract '{}'", url);
    let body = reqwest::get(url).await?.text().await?;
    let images = get_album_images(&body).await?;
    let title = get_album_title(&body).await?;
    let size = get_album_size(&body).await?;
    Ok((title, images, size))
}

pub async fn get_album_images(body: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let fragment = Html::parse_document(&body);
    let selector = Selector::parse(TABLE).unwrap();
    let mut v = Vec::<String>::new();
    for elem in fragment.select(&selector) {
        v.push(elem.value().attr("href").unwrap().to_string());
    }
    Ok(v)
}

pub async fn get_album_title(body: &str) -> Result<String, Box<dyn Error>> {
    let fragment = Html::parse_document(&body);
    let selector = Selector::parse(H1).unwrap();
    let title = fragment
        .select(&selector)
        .next()
        .expect("album not found")
        .inner_html()
        .trim()
        .to_string();
    Ok(title)
}

pub async fn get_album_size(body: &str) -> Result<String, Box<dyn Error>> {
    let fragment = Html::parse_document(&body);
    let selector = Selector::parse(SIZE).unwrap();
    let title = fragment
        .select(&selector)
        .next()
        .unwrap()
        .inner_html()
        .trim()
        .to_string();
    Ok(title)
}

async fn create_dir<P: AsRef<Path>>(path: P) {
    tokio::fs::create_dir_all(path)
        .await
        .unwrap_or_else(|e| panic!("Error creating dir: {}", e));
}
/*
pub async fn download_image(
    dir: &String,
    url: &String,
    client: &reqwest::Client,
) -> Result<u128, Box<dyn Error>> {
    let fname = image_name_from_url(url).await?;
    let dest = dir.to_owned() + &fname;
    let resp = client.get(url).send().await?.bytes().await?;
    let mut reader: &[u8] = &resp;
    let mut file = File::create(&dest).await?;
    let bytes = io::copy(&mut reader, &mut file).await?;
    let bytes = Byte::from_bytes(bytes.into());
    Ok(bytes.into())
}
*/

pub async fn download_image2(
    dir: &String,
    url: &String,
    client: &reqwest::Client,
) -> Result<(String, String, u128, Bytes), Box<dyn Error>> {
    let fname = image_name_from_url(url).await?;
    let path = dir.clone();
    let resp = client.get(url).send().await?.bytes().await?;
    let size = resp.len();
    Ok((fname, path, size.try_into().unwrap(), resp))
}

pub async fn image_name_from_url(url: &String) -> Result<String, Box<dyn Error>> {
    let parsed_url = reqwest::Url::parse(url)?;
    Ok(parsed_url.path().to_string())
}
