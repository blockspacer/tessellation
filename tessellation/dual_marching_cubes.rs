use xplicit_primitive::Object;
use {BitSet, Mesh, Plane, qef};
use dual_marching_cubes_cell_configs::get_dmc_cell_configs;
use xplicit_types::{Float, Point, Vector};
use std::collections::HashMap;
use std::cell::{Cell, RefCell};
use std::{error, fmt};
use std::cmp;
use cgmath::{Array, EuclideanSpace};
use rand;

// How accurately find zero crossings.
const PRECISION: Float = 0.05;

pub type Index = [usize; 3];

fn offset(idx: Index, offset: Index) -> Index {
    [idx[0] + offset[0], idx[1] + offset[1], idx[2] + offset[2]]
}

fn neg_offset(idx: Index, offset: Index) -> Index {
    [idx[0] - offset[0], idx[1] - offset[1], idx[2] - offset[2]]
}


//  Corner indexes
//
//      6---------------7
//     /|              /|
//    / |             / |
//   /  |            /  |
//  4---------------5   |
//  |   |           |   |
//  |   2-----------|---3
//  |  /            |  /
//  | /             | /
//  |/              |/
//  0---------------1
#[derive(Clone, Copy)]
pub enum Corner {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
}
// Corner connections
pub const CORNER_CONNS: [[Corner; 3]; 8] = [[Corner::B, Corner::C, Corner::E],
                                            [Corner::A, Corner::D, Corner::F],
                                            [Corner::A, Corner::D, Corner::G],
                                            [Corner::B, Corner::C, Corner::H],
                                            [Corner::A, Corner::F, Corner::G],
                                            [Corner::B, Corner::E, Corner::H],
                                            [Corner::C, Corner::E, Corner::H],
                                            [Corner::D, Corner::F, Corner::G]];

// Which corners does a edge connect:
pub const EDGE_DEF: [(Corner, Corner); 12] = [(Corner::A, Corner::B),
                                              (Corner::A, Corner::C),
                                              (Corner::A, Corner::E),
                                              (Corner::C, Corner::D),
                                              (Corner::B, Corner::D),
                                              (Corner::B, Corner::F),
                                              (Corner::E, Corner::F),
                                              (Corner::E, Corner::G),
                                              (Corner::C, Corner::G),
                                              (Corner::G, Corner::H),
                                              (Corner::F, Corner::H),
                                              (Corner::D, Corner::H)];
//  Edge indexes
//
//      +-------9-------+
//     /|              /|
//    7 |            10 |              ^
//   /  8            /  11            /
//  +-------6-------+   |     ^    higher indexes in y
//  |   |           |   |     |     /
//  |   +-------3---|---+     |    /
//  2  /            5  /  higher indexes
//  | 1             | 4      in z
//  |/              |/        |/
//  o-------0-------+         +-- higher indexes in x ---->
//
// Point o is the reference point of the current cell.
// All edges go from lower indexes to higher indexes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Edge {
    A = 0,
    B = 1,
    C = 2,
    D = 3,
    E = 4,
    F = 5,
    G = 6,
    H = 7,
    I = 8,
    J = 9,
    K = 10,
    L = 11,
}

impl Edge {
    pub fn from_usize(e: usize) -> Edge {
        match e {
            0 => Edge::A,
            1 => Edge::B,
            2 => Edge::C,
            3 => Edge::D,
            4 => Edge::E,
            5 => Edge::F,
            6 => Edge::G,
            7 => Edge::H,
            8 => Edge::I,
            9 => Edge::J,
            10 => Edge::K,
            11 => Edge::L,
            _ => panic!("Not edge for {:?}", e),
        }
    }
    pub fn base(&self) -> Edge {
        Edge::from_usize(*self as usize % 3)
    }
}

// Cell offsets of edges
const EDGE_OFFSET: [Index; 12] = [[0, 0, 0], [0, 0, 0], [0, 0, 0], [0, 1, 0], [1, 0, 0],
                                  [1, 0, 0], [0, 0, 1], [0, 0, 1], [0, 1, 0], [0, 1, 1],
                                  [1, 0, 1], [1, 1, 0]];

// Quad definition for edges 0-2.
const QUADS: [[Edge; 4]; 3] = [[Edge::A, Edge::G, Edge::J, Edge::D],
                               [Edge::B, Edge::E, Edge::K, Edge::H],
                               [Edge::C, Edge::I, Edge::L, Edge::F]];

#[derive(Debug)]
enum DualContouringError {
    HitZero(Point),
}

impl error::Error for DualContouringError {
    fn description(&self) -> &str {
        match self {
            &DualContouringError::HitZero(_) => "Hit zero value during grid sampling.",
        }
    }
}

impl fmt::Display for DualContouringError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &DualContouringError::HitZero(p) => write!(f, "Hit zero value for {:?}", p),
        }
    }
}

pub struct DualMarchingCubes {
    object: Box<Object>,
    origin: Point,
    dim: [usize; 3],
    mesh: RefCell<Mesh>,
    // Map (EdgeSet, Index) -> index in mesh.vertices
    vertex_map: RefCell<HashMap<(BitSet, Index), usize>>,
    res: Float,
    value_grid: HashMap<Index, Float>,
    edge_grid: RefCell<HashMap<(Edge, Index), Plane>>,
    cell_configs: Vec<Vec<BitSet>>,
    qefs: Cell<usize>,
    clamps: Cell<usize>,
}

// Returns the next largest power of 2
fn pow2roundup(x: usize) -> usize {
    let mut x = x;
    x -= 1;
    x |= x >> 1;
    x |= x >> 2;
    x |= x >> 4;
    x |= x >> 8;
    x |= x >> 16;
    x |= x >> 32;
    return x + 1;
}

impl DualMarchingCubes {
    // Constructor
    // obj: Object to tessellate
    // res: resolution
    pub fn new(obj: Box<Object>, res: Float) -> DualMarchingCubes {
        let bbox = obj.bbox().dilate(1. + res * 1.1);
        println!("DualMarchingCubes: res: {:} {:?}", res, bbox);
        DualMarchingCubes {
            object: obj,
            origin: bbox.min,
            dim: [(bbox.dim()[0] / res).ceil() as usize,
                  (bbox.dim()[1] / res).ceil() as usize,
                  (bbox.dim()[2] / res).ceil() as usize],
            mesh: RefCell::new(Mesh {
                vertices: Vec::new(),
                faces: Vec::new(),
            }),
            vertex_map: RefCell::new(HashMap::new()),
            res: res,
            value_grid: HashMap::new(),
            edge_grid: RefCell::new(HashMap::new()),
            cell_configs: get_dmc_cell_configs(),
            qefs: Cell::new(0),
            clamps: Cell::new(0),
        }
    }
    pub fn tesselate(&mut self) -> Mesh {
        loop {
            match self.try_tesselate() {
                Ok(mesh) => return mesh,
                Err(x) => {
                    let padding = self.res / (10. + rand::random::<f64>().abs());
                    println!("Error: {:?}. moving by {:?} and retrying.", x, padding);
                    self.origin.x -= padding;
                    self.value_grid.clear();
                    self.mesh.borrow_mut().vertices.clear();
                    self.mesh.borrow_mut().faces.clear();
                    self.qefs.set(0);
                    self.clamps.set(0);
                }
            }
        }
    }

    fn sample_value_grid(&mut self,
                         idx: Index,
                         pos: Point,
                         size: usize,
                         val: Float)
                         -> Option<DualContouringError> {
        debug_assert!(size > 1);
        let mut midx = idx;
        let size = size / 2;
        let vpos = [pos,
                    Point::new(pos.x + size as Float * self.res,
                               pos.y + size as Float * self.res,
                               pos.z + size as Float * self.res)];
        let sub_cube_diagonal = size as Float * self.res * 3_f64.sqrt();

        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let mpos = Point::new(vpos[x].x, vpos[y].y, vpos[z].z);
                    let value = if midx == idx {
                        val
                    } else {
                        self.object.approx_value(mpos, self.res)
                    };

                    if value == 0. {
                        return Some(DualContouringError::HitZero(mpos));
                    }

                    if size > 1 && value.abs() <= sub_cube_diagonal {
                        if let Some(e) = self.sample_value_grid(midx, mpos, size, value) {
                            return Some(e);
                        }
                    } else {
                        self.value_grid.insert(midx, value);
                    }
                    midx[0] += size;
                }
                midx[0] -= 2 * size;
                midx[1] += size;
            }
            midx[1] -= 2 * size;
            midx[2] += size;
        }
        None
    }

    // This method does the main work of tessellation.
    fn try_tesselate(&mut self) -> Result<Mesh, DualContouringError> {
        let res = self.res;
        let t1 = ::time::now();

        let maxdim = cmp::max(self.dim[0], cmp::max(self.dim[1], self.dim[2]));
        let origin = self.origin;
        let origin_value = self.object.approx_value(origin, res);

        if let Some(e) = self.sample_value_grid([0, 0, 0],
                                                origin,
                                                pow2roundup(maxdim),
                                                origin_value) {
            return Err(e);
        }

        let t2 = ::time::now();
        println!("generated value_grid: {:}", t2 - t1);
        println!("value_grid with {:} for {:} cells.",
                 self.value_grid.len(),
                 self.dim[0] * self.dim[1] * self.dim[2]);

        // Store crossing positions of edges in edge_grid
        {
            let mut edge_grid = self.edge_grid.borrow_mut();
            for (point_idx, point_value) in &self.value_grid {
                for edge in [Edge::A, Edge::B, Edge::C].iter() {
                    let mut adjacent_idx = point_idx.clone();
                    adjacent_idx[*edge as usize] += 1;
                    if let Some(adjacent_value) = self.value_grid
                                                      .get(&adjacent_idx) {
                        let point_pos = self.origin +
                                        res *
                                        Vector::new(point_idx[0] as Float,
                                                    point_idx[1] as Float,
                                                    point_idx[2] as Float);
                        let mut adjacent_pos = point_pos;
                        adjacent_pos[*edge as usize] += res;
                        if let Some(plane) = self.find_zero(point_pos,
                                                            *point_value,
                                                            adjacent_pos,
                                                            *adjacent_value) {
                            edge_grid.insert((*edge, *point_idx), plane);
                        }
                    }
                }
            }
        }
        let t3 = ::time::now();
        println!("generated edge_grid: {:}", t3 - t2);

        for &(edge_index, ref idx) in self.edge_grid.borrow().keys() {
            self.compute_quad(edge_index, *idx);
        }
        let t4 = ::time::now();
        println!("generated quads: {:}", t4 - t3);

        println!("qefs: {:?} clamps: {:?}", self.qefs, self.clamps);

        println!("computed mesh with {:?} faces.",
                 self.mesh.borrow().faces.len());

        Ok(self.mesh.borrow().clone())
    }

    fn get_edge_tangent_plane(&self, edge: Edge, cell_idx: Index) -> Plane {
        let data_idx = offset(cell_idx, EDGE_OFFSET[edge as usize]);
        let data_edge = edge.base();
        if let Some(ref plane) = self.edge_grid
                                     .borrow()
                                     .get(&(edge.base(), data_idx)) {
            return *plane.clone();
        }
        panic!("could not find edge_point: {:?} {:?},-> {:?} {:?}",
               edge,
               data_edge,
               cell_idx,
               data_idx);
    }

    // Return the Point index (in self.mesh.vertices) the the point belonging to edge/idx.
    fn lookup_cell_point(&self, edge: Edge, idx: Index) -> usize {
        let edge_set = self.get_connected_edges(edge, self.bitset_for_cell(idx));
        // Try to lookup the edge_set for this index.
        if let Some(index) = self.vertex_map.borrow().get(&(edge_set, idx)) {
            return *index;
        }
        // It does not exist. So calculate all edge crossings and their normals.
        let point = self.compute_cell_point(edge_set, idx);

        let ref mut vertex_list = self.mesh.borrow_mut().vertices;
        let result = vertex_list.len();
        vertex_list.push([point.x, point.y, point.z]);
        return result;
    }

    fn compute_cell_point(&self, edge_set: BitSet, idx: Index) -> Point {
        let tangent_planes: Vec<_> = edge_set.into_iter()
                                             .map(|edge| {
                                                 self.get_edge_tangent_plane(Edge::from_usize(edge),
                                                                             idx)
                                             })
                                             .collect();

        // Fit the point to tangent planes.
        let mut qef = qef::Qef::new(&tangent_planes);
        qef.solve();
        let qef_solution = Point::new(qef.solution[0], qef.solution[1], qef.solution[2]);

        if self.is_in_cell(&idx, &qef_solution) {
            let qefs = self.qefs.get();
            self.qefs.set(qefs + 1);
            return qef_solution;
        }
        let mean = Point::from_vec(&tangent_planes.iter()
                                                  .fold(Vector::new(0., 0., 0.),
                                                        |sum, x| sum + x.p.to_vec()) /
                                   tangent_planes.len() as Float);
        // Proper calculation landed us outside the cell.
        // Revert mean.
        let clamps = self.clamps.get();
        self.clamps.set(clamps + 1);
        return mean;
    }

    fn is_in_cell(&self, idx: &Index, p: &Point) -> bool {
        idx.iter().enumerate().all(|(i, &idx_)| {
            let d = p[i] - self.origin[i] - idx_ as Float * self.res;
            d > 0. && d < self.res
        })
    }

    fn bitset_for_cell(&self, idx: Index) -> BitSet {
        let mut idx = idx;
        let mut result = BitSet::new(0);
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    if let Some(v) = self.value_grid.get(&idx) {
                        if *v < 0. {
                            result.set(z << 2 | y << 1 | x);
                        }
                    }
                    idx[0] += 1;
                }
                idx[0] -= 2;
                idx[1] += 1;
            }
            idx[1] -= 2;
            idx[2] += 1;
        }
        result
    }

    // Return a BitSet containing all egdes connected to "edge" in this cell.
    fn get_connected_edges(&self, edge: Edge, cell: BitSet) -> BitSet {
        for edge_set in self.cell_configs[cell.as_usize()].iter() {
            if edge_set.get(edge as usize) {
                return *edge_set;
            }
        }
        panic!("Did not find edge_set for {:?} and {:?}", edge, cell);
    }

    // Compute a quad for the given edge and append it to the list.
    fn compute_quad(&self, edge: Edge, idx: Index) {
        debug_assert!((edge as usize) < 4);
        debug_assert!(idx.iter().all(|&i| i > 0));

        let mut p = Vec::with_capacity(4);
        for quad_egde in QUADS[edge as usize].iter() {
            p.push(self.lookup_cell_point(*quad_egde,
                                          neg_offset(idx, EDGE_OFFSET[*quad_egde as usize])))
        }
        if let Some(v) = self.value_grid.get(&idx) {
            if *v < 0. {
                p.reverse();
            }
        }
        let ref mut face_list = self.mesh.borrow_mut().faces;
        face_list.push([p[0], p[1], p[2]]);
        face_list.push([p[2], p[3], p[0]]);
    }

    // If a is inside the object and b outside - this method return the point on the line between
    // a and b where the object edge is. It also returns the normal on that point.
    // av and bv represent the object values at a and b.
    fn find_zero(&self, a: Point, av: Float, b: Point, bv: Float) -> Option<(Plane)> {
        debug_assert!(av == self.object.approx_value(a, self.res));
        debug_assert!(bv == self.object.approx_value(b, self.res));
        assert!(a != b);
        if av.signum() == bv.signum() {
            return None;
        }
        let mut distance = (a - b).min().abs().max((a - b).max());
        distance = distance.min(av.abs()).min(bv.abs());
        if distance < PRECISION * self.res {
            let mut result = &a;
            if bv.abs() < av.abs() {
                result = &b;
            }
            return Some(Plane {
                p: *result,
                n: self.object.normal(*result),
            });
        }
        // Linear interpolation of the zero crossing.
        let n = a + (b - a) * (av.abs() / (bv - av).abs());
        let nv = self.object.approx_value(n, self.res);

        if av.signum() != nv.signum() {
            return self.find_zero(a, av, n, nv);
        } else {
            return self.find_zero(n, nv, b, bv);
        }
    }
}