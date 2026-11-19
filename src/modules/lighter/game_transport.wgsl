            // One additional lobe per eligible first surface at 50% trace
            // extent. Reflection has priority; other pixels may take a short
            // diffuse bounce over the direct path's ambient/lightmap base.
            if game_opaque(s,path.medium,path.purpose) {
                result += path.weight*color;
                if terminal { break; }
                let b = material_bsdf(s);
                let nv = clamp(dot(s.n,-ray.dir),0.,1.);
                let spec = (frame.options.x & REFLECTIONS)!=0u && strongest(b.f0)>0.015;
                let roughness = sqrt(b.alpha);
                let probe = frame.sky0.x!=0u;
                if spec && (roughness<=0.35 || (s.pbr.y>=0. && s.pbr.x>=0.5 && roughness<=0.55)) {
                    let basis = bsdf_basis(s.n);
                    var d = reflect(ray.dir,s.n);
                    if frame.ray_effects.x>0. && solid.t<=frame.ray_effects.x && !(probe && roughness<0.06) && nv>0. {
                        let h = basis*ggx_visible_normal(transpose(basis)*(-ray.dir),b.alpha,vec2<f32>(random(seed),random(seed)));
                        d = reflect(ray.dir,h);
                        let nl = dot(s.n,d);
                        if nl>0. && dot(s.face,d)>0. {
                            path.ray = secondary(p,d,s.face,ray,solid.t);
                            path.ray.high = min(path.ray.high,frame.ray_effects.x);
                            path.weight *= fresnel_rgb(b.f0,dot(-ray.dir,h))*ggx_g1(nl,b.alpha);
                            path.purpose = SPECULAR;
                            temporal_specular_leg(true);
                            path.depth++;
                            continue;
                        }
                    } else if probe && dot(d,s.face)>0. {
                        result += path.weight*fresnel_rgb(b.f0,nv)*environment(secondary(p,d,s.face,ray,solid.t),true);
                    }
                }
                if (s.flags & BAKED)==0u && (frame.options.x & LIGHTING)!=0u &&
                    frame.ray_effects.z>0. && solid.t<=frame.ray_effects.z && strongest(b.diffuse)>0. {
                    let d = hemisphere(s.n,seed);
                    if dot(d,s.face)>0. {
                        path.ray = secondary(p,d,s.face,ray,solid.t);
                        path.ray.high = min(path.ray.high,frame.ray_effects.z);
                        path.weight *= 0.25*b.diffuse*(1.-fresnel_rgb(b.f0,nv));
                        path.purpose = DIFFUSE;
                        path.depth++;
                        continue;
                    }
                }
                break;
            }
