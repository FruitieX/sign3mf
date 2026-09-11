#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn distance(self, other: Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

pub type Contour = Vec<Point>;

pub fn signed_area(poly: &[Point]) -> f64 {
    if poly.len() < 3 {
        return 0.0;
    }
    poly.iter()
        .enumerate()
        .map(|(i, p)| {
            let q = poly[(i + 1) % poly.len()];
            p.x * q.y - q.x * p.y
        })
        .sum::<f64>()
        * 0.5
}

pub fn area(poly: &[Point]) -> f64 {
    signed_area(poly).abs()
}

pub fn simplify_collinear(poly: &mut Contour, eps: f64) {
    if poly.len() < 3 {
        return;
    }
    let mut changed = true;
    while changed && poly.len() > 3 {
        changed = false;
        let n = poly.len();
        for i in 0..n {
            let a = poly[(i + n - 1) % n];
            let b = poly[i];
            let c = poly[(i + 1) % n];
            let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
            if cross.abs() <= eps && b.distance(a) + b.distance(c) <= a.distance(c) + eps {
                poly.remove(i);
                changed = true;
                break;
            }
        }
    }
}

pub fn rdp(points: &[Point], epsilon: f64) -> Contour {
    if points.len() <= 3 || epsilon <= 0.0 {
        return points.to_vec();
    }
    let a = points[0];
    let b = *points.last().unwrap();
    let mut max_dist = 0.0;
    let mut index = 0;
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    for (i, p) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = if len2 == 0.0 {
            p.distance(a)
        } else {
            ((p.x - a.x) * dy - (p.y - a.y) * dx).abs() / len2.sqrt()
        };
        if d > max_dist {
            max_dist = d;
            index = i;
        }
    }
    if max_dist > epsilon {
        let mut result = rdp(&points[..=index], epsilon);
        let right = rdp(&points[index..], epsilon);
        result.pop();
        result.extend(right);
        result
    } else {
        vec![a, b]
    }
}

pub fn rdp_closed(points: &[Point], epsilon: f64) -> Contour {
    if points.len() < 4 || epsilon <= 0.0 {
        return points.to_vec();
    }
    let mut expanded = points.to_vec();
    expanded.push(points[0]);
    let mut reduced = rdp(&expanded, epsilon);
    if reduced
        .last()
        .is_some_and(|p| p.distance(reduced[0]) < 1e-9)
    {
        reduced.pop();
    }
    reduced
}

pub fn point_in_polygon(p: Point, poly: &[Point]) -> bool {
    let mut inside = false;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

pub fn bounds(contours: &[Contour]) -> Option<(f64, f64, f64, f64)> {
    let mut it = contours.iter().flat_map(|c| c.iter());
    let first = *it.next()?;
    let mut minx = first.x;
    let mut maxx = first.x;
    let mut miny = first.y;
    let mut maxy = first.y;
    for p in it {
        minx = minx.min(p.x);
        maxx = maxx.max(p.x);
        miny = miny.min(p.y);
        maxy = maxy.max(p.y);
    }
    Some((minx, miny, maxx, maxy))
}
