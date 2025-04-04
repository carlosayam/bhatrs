use burn::{
    module::{
        Module,
        Param,
    },
    optim::{AdamConfig, GradientsParams, Optimizer},
    prelude::{
        Backend, Config, Tensor
    },
    tensor::backend::AutodiffBackend,
};

use core::f64;
use std::f64::consts::PI;

use rand_chacha::ChaCha8Rng;
use rand_core::SeedableRng;

use rand::distributions::Distribution;
use statrs::distribution::Cauchy;
use linfa_nn::{BallTree, distance::L1Dist, NearestNeighbour};
use ndarray::{Array, array};

use burn::tensor::ElementConversion;

#[derive(Module, Debug)]
pub struct BHatModel<B: Backend> {
    loc: Param<Tensor<B, 1>>,
    scale: Param<Tensor<B, 1>>,
}

impl<B: AutodiffBackend> BHatModel<B> {
    pub fn forward(&self, data: &Tensor<B, 1>, balls: &Tensor<B, 1>, factor: f64) -> Tensor<B, 1> {
        // calculate Pdf_{Cauchy(l,s)}(data) =
        // \frac{1}{\pi s (1 + (\frac{x - l}{s})^2)}; l = loc, s = scale
        let v = (self.loc.val() - data.clone()) / self.scale.val();
        let v = v.powi_scalar(2);
        let v = (v + 1.0) * self.scale.val() * PI;
        let pdf = v.powi_scalar(-1);
        // now calculate Hellinger Distance squared estimator, eqs (2) & (3) in paper
        let v = (pdf * balls.clone()).powf_scalar(0.5);
        v.sum() * (-factor) + 1.0
    }
}

fn calculate_balls<B: Backend>(data: &Vec<f64>, device: &B::Device) -> (Tensor<B, 1>, Tensor<B, 1>) {

    // considered that the sample could be split to ensure i.i.d terms in the sum
    // but there were no apparent benefits; leaving this legacy in case need to investigate
    // again
    let data1 = &data[..];  // slice used for calculate volume to nearest
    let data2 = &data[..];  // slice used to iterate points

    let algo = BallTree::new();
    let arr = Array::from_shape_vec([data1.len(), 1], data1.to_vec()).unwrap();
    let arr = arr.view();
    let nn_index = algo.from_batch(&arr, L1Dist).unwrap();
    let pos_nearest = 1;

    let radii: Vec<f64> = data2.iter()
        .map(|pt: &f64| (nn_index.k_nearest((array![*pt]).view(), pos_nearest + 1).unwrap(), pt))
        .map(|resp: (Vec<(ndarray::ArrayBase<ndarray::ViewRepr<&f64>, ndarray::Dim<[usize; 1]>>, usize)>, &f64)|
                    (resp.1 - resp.0[pos_nearest].0[0]).abs())  // distance to nearest neighbour
        .map(|v: f64| v * 2.0)                        // ball volume in dimension 1
        .collect();

    (
        Tensor::from_data(data2, device),
        Tensor::from_data(radii.as_slice(), device)
    )
}

fn min_median_max(numbers: &Vec<f64>) -> (f64, f64, f64) {

    let mut to_sort = numbers.clone();
    to_sort.sort_by(|a, b| a.partial_cmp(b).unwrap());
    
    let mid = numbers.len() / 2;
    let med = if numbers.len() % 2 == 0 {
        (numbers[mid - 1] + numbers[mid]) / 2.0
    } else {
        numbers[mid]
    };
    (to_sort[0], med, to_sort[numbers.len()-1])
}

#[derive(Config)]
pub struct TrainingConfig {

    #[config(default = 1000)]
    pub num_runs: usize,

    #[config(default = 0.25)]
    pub lr: f64,

    pub config_optimizer: AdamConfig,
}

pub fn run<B: AutodiffBackend>(
    num: usize,
    seed: Option<u64>,
    device: B::Device,
) {
    // some global refs
    let config_optimizer = AdamConfig::new();
    let config = TrainingConfig::new(config_optimizer);

    let mut rng: ChaCha8Rng = match seed {
        Some(val) => ChaCha8Rng::seed_from_u64(val),
        None => ChaCha8Rng::from_entropy(),
    };

    // create random vec
    let dist: Cauchy = Cauchy::new(20.0, 3.0).unwrap();
    let vec = Vec::from_iter((0..num).map(|_| dist.sample(&mut rng)));

    let (v_min, v_med, v_max) = min_median_max(&vec);
    let balls = calculate_balls::<B>(&vec, &device);
    let factor = 2.0 / ((num as f64) * PI).sqrt();

    let loc = Tensor::from_data([v_med], &device);
    let scale = Tensor::from_data([(v_max - v_min) / (num as f64)], &device);

    let mut model = BHatModel {
        loc: Param::from_tensor(loc),
        scale: Param::from_tensor(scale),
    };
    println!("Sample size: {}", vec.len());
    println!("Starting params");
    println!("Loc: {}", model.loc.val().clone().into_scalar());
    println!("Scale: {}\n", model.scale.val().clone().into_scalar());

    let mut optimizer = config.config_optimizer.init();
    let epsilon: f64 = 0.000001;

    let mut ix = 1;
    while ix <= config.num_runs {

        let hd = model.forward(&balls.0, &balls.1, factor);

        let grads = hd.backward();

        let grads_container = GradientsParams::from_grads(grads, &model);

        let loc_grad = grads_container.get::<B::InnerBackend, 1>(model.loc.id).unwrap();
        let scale_grad = grads_container.get::<B::InnerBackend, 1>(model.scale.id).unwrap();
        let loc_grad: f64 = loc_grad.into_scalar().elem();
        let scale_grad: f64 = scale_grad.into_scalar().elem();

        model = optimizer.step(config.lr, model, grads_container);
        let bhat_val: f64 = hd.into_scalar().elem::<f64>();

        if ix % 10 == 0 {
            println!("HD^2: {} ({})", bhat_val, ix);
        }
        if loc_grad.abs() < epsilon && scale_grad.abs() < epsilon {
            break;
        }
        ix += 1;
    }

    println!("\nEnd params (iterations={})", ix);
    println!("Loc: {}", model.loc.val().clone().into_scalar());
    println!("Scale: {}", model.scale.val().clone().into_scalar());
}
