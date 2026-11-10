use crate::db::FolderStat;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug)]
pub enum TileKind {
    Folder(String),
    Direct,
    Other,
}

#[derive(Clone, Debug)]
pub struct Tile {
    pub label: String,
    pub count: i64,
    pub bytes: i64,
    pub unique_bytes: i64,
    pub kind: TileKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric { Bytes, Count, UniqueBytes, DuplicateWaste }

impl Default for Metric { fn default() -> Self { Self::Bytes } }

impl Metric {
    pub fn value(self, tile: &Tile) -> i64 {
        match self {
            Self::Bytes => tile.bytes,
            Self::Count => tile.count,
            Self::UniqueBytes => tile.unique_bytes,
            Self::DuplicateWaste => (tile.bytes - tile.unique_bytes).max(0),
        }
    }
}

fn parent(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .filter(|parent| *parent != Path::new(path))
        .map(|parent| parent.to_string_lossy().into_owned())
}

#[derive(Default)]
pub struct FolderTree {
    by_bytes: HashMap<Option<String>, Vec<Tile>>,
    by_count: HashMap<Option<String>, Vec<Tile>>,
    by_unique: HashMap<Option<String>, Vec<Tile>>,
    by_waste: HashMap<Option<String>, Vec<Tile>>,
    totals: HashMap<String, (i64, i64, i64)>,
    global_total: (i64, i64, i64),
}

impl FolderTree {
    pub fn new(stats: &[FolderStat]) -> Self {
        let mut tree = Self::default();
        for stat in stats {
            tree.global_total.0 += stat.count;
            tree.global_total.1 += stat.bytes;
            tree.global_total.2 += stat.unique_bytes;
            let mut path = Some(stat.path.clone());
            while let Some(current) = path {
                let entry = tree.totals.entry(current.clone()).or_default();
                entry.0 += stat.count;
                entry.1 += stat.bytes;
                entry.2 += stat.unique_bytes;
                path = parent(&current);
            }
            tree.by_bytes
                .entry(Some(stat.path.clone()))
                .or_default()
                .push(Tile {
                    label: "Images here".into(),
                    count: stat.count,
                    bytes: stat.bytes,
                    unique_bytes: stat.unique_bytes,
                    kind: TileKind::Direct,
                });
        }
        for (path, (count, bytes, unique_bytes)) in &tree.totals {
            let folder_parent = parent(path).filter(|parent| tree.totals.contains_key(parent));
            let label = Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            tree.by_bytes.entry(folder_parent).or_default().push(Tile {
                label,
                count: *count,
                bytes: *bytes,
                unique_bytes: *unique_bytes,
                kind: TileKind::Folder(path.clone()),
            });
        }
        for items in tree.by_bytes.values_mut() {
            items
                .sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
        }
        tree.by_count = tree.by_bytes.clone();
        for items in tree.by_count.values_mut() {
            items
                .sort_unstable_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        }
        tree.by_unique = tree.by_bytes.clone();
        for items in tree.by_unique.values_mut() {
            items.sort_unstable_by(|a, b| b.unique_bytes.cmp(&a.unique_bytes).then_with(|| a.label.cmp(&b.label)));
        }
        tree.by_waste = tree.by_bytes.clone();
        for items in tree.by_waste.values_mut() {
            items.sort_unstable_by(|a, b| (b.bytes-b.unique_bytes).cmp(&(a.bytes-a.unique_bytes)).then_with(|| a.label.cmp(&b.label)));
        }
        tree
    }

    fn entries(&self, root: Option<&str>, metric: Metric) -> &[Tile] {
        let key = root.map(str::to_owned);
        (match metric { Metric::Bytes => &self.by_bytes, Metric::Count => &self.by_count,
            Metric::UniqueBytes => &self.by_unique, Metric::DuplicateWaste => &self.by_waste })
        .get(&key)
        .map(Vec::as_slice)
        .unwrap_or_default()
    }

    pub fn tiles(&self, root: Option<&str>, metric: Metric) -> Vec<Tile> {
        let items = self.entries(root, metric);
        if items.len() <= 48 {
            return items.to_vec();
        }
        let mut shown = items[..47].to_vec();
        let total = root
            .and_then(|root| self.totals.get(root).copied())
            .unwrap_or(self.global_total);
        shown.push(Tile {
            label: format!("Other ({} items)", items.len() - 47),
            count: total.0 - shown.iter().map(|tile| tile.count).sum::<i64>(),
            bytes: total.1 - shown.iter().map(|tile| tile.bytes).sum::<i64>(),
            unique_bytes: total.2 - shown.iter().map(|tile| tile.unique_bytes).sum::<i64>(),
            kind: TileKind::Other,
        });
        shown
    }

    pub fn other(&self, root: Option<&str>, metric: Metric) -> &[Tile] {
        self.entries(root, metric).get(47..).unwrap_or_default()
    }
}

fn weight(tile: &Tile, metric: Metric) -> f32 {
    metric.value(tile).max(1) as f32
}

pub fn layout(tiles: &[Tile], rect: egui::Rect, metric: Metric) -> Vec<(Tile, egui::Rect)> {
    fn split(
        tiles: &[Tile],
        rect: egui::Rect,
        metric: Metric,
        result: &mut Vec<(Tile, egui::Rect)>,
    ) {
        if tiles.is_empty() || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        if tiles.len() == 1 {
            result.push((tiles[0].clone(), rect));
            return;
        }
        let total: f32 = tiles.iter().map(|tile| weight(tile, metric)).sum();
        let mut left = 0.0;
        let mut cut = 0;
        while cut + 1 < tiles.len() && left < total * 0.5 {
            left += weight(&tiles[cut], metric);
            cut += 1;
        }
        let fraction = (left / total).clamp(0.01, 0.99);
        if rect.width() >= rect.height() {
            let x = rect.left() + rect.width() * fraction;
            split(
                &tiles[..cut],
                egui::Rect::from_min_max(rect.min, egui::pos2(x, rect.bottom())),
                metric,
                result,
            );
            split(
                &tiles[cut..],
                egui::Rect::from_min_max(egui::pos2(x, rect.top()), rect.max),
                metric,
                result,
            );
        } else {
            let y = rect.top() + rect.height() * fraction;
            split(
                &tiles[..cut],
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), y)),
                metric,
                result,
            );
            split(
                &tiles[cut..],
                egui::Rect::from_min_max(egui::pos2(rect.left(), y), rect.max),
                metric,
                result,
            );
        }
    }
    let mut result = Vec::with_capacity(tiles.len());
    split(tiles, rect, metric, &mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_accumulate_and_metric_changes_order() {
        let stats = vec![
            FolderStat {
                path: "C:\\Photos\\A".into(),
                count: 10,
                bytes: 10,
                unique_bytes: 10,
            },
            FolderStat {
                path: "C:\\Photos\\B".into(),
                count: 1,
                bytes: 100,
                unique_bytes: 80,
            },
        ];
        let tree = FolderTree::new(&stats);
        let count = tree.tiles(Some("C:\\Photos"), Metric::Count);
        let bytes = tree.tiles(Some("C:\\Photos"), Metric::Bytes);
        assert_eq!(count[0].label, "A");
        assert_eq!(bytes[0].label, "B");
        assert_eq!(count.iter().map(|tile| tile.count).sum::<i64>(), 11);
    }
}
