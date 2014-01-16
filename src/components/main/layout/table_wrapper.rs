/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! CSS block formatting contexts.

use layout::box::Box;
use layout::context::LayoutContext;
use layout::display_list_builder::{DisplayListBuilder, ExtraDisplayListData};
use layout::flow::{TableWrapperFlowClass, FlowClass, Flow, FlowData, ImmutableFlowUtils};
use layout::flow;
use layout::model::{MaybeAuto, Specified, Auto, specified_or_none, specified};
use layout::float_context::{FloatContext, PlacementInfo, Invalid, FloatType};

use std::cell::Cell;
use style::computed_values::table_layout;
use geom::{Point2D, Rect, SideOffsets2D};
use gfx::display_list::DisplayList;
use servo_util::geometry::Au;
use servo_util::geometry;

/// Information specific to floated blocks.
pub struct FloatedBlockInfo {
    containing_width: Au,

    /// Offset relative to where the parent tried to position this flow
    rel_pos: Point2D<Au>,

    /// Index into the box list for inline floats
    index: Option<uint>,

    /// Number of floated children
    floated_children: uint,

    /// Left or right?
    float_type: FloatType
}

impl FloatedBlockInfo {
    pub fn new(float_type: FloatType) -> FloatedBlockInfo {
        FloatedBlockInfo {
            containing_width: Au(0),
            rel_pos: Point2D(Au(0), Au(0)),
            index: None,
            floated_children: 0,
            float_type: float_type
        }
    }
}

/// A table formatting context.
pub struct TableWrapperFlow {
    /// Data common to all flows.
    base: FlowData,

    /// The associated box.
    box: Option<Box>,

    /// Additional floating flow members.
    float: Option<~FloatedBlockInfo>,

    col_boxes: ~[Box],
    col_widths: ~[Au],
    is_fixed_layout: bool,
}

impl TableWrapperFlow {
    pub fn new(base: FlowData) -> TableWrapperFlow {
        TableWrapperFlow {
            base: base,
            box: None,
            float: None,
            col_boxes: ~[],
            col_widths: ~[],
            is_fixed_layout: false,
        }
    }

    pub fn from_box(base: FlowData, box: Box) -> TableWrapperFlow {
        let is_fixed_layout = box.style().Table.table_layout == table_layout::fixed;
        TableWrapperFlow {
            base: base,
            box: Some(box),
            float: None,
            col_boxes: ~[],
            col_widths: ~[],
            is_fixed_layout: is_fixed_layout,
        }
    }

    pub fn is_float(&self) -> bool {
        self.float.is_some()
    }

    pub fn teardown(&mut self) {
        for box in self.box.iter() {
            box.teardown();
        }
        self.box = None;
        self.float = None;
    }

    /// Computes left and right margins and width based on CSS 2.1 section 10.3.3.
    /// Requires borders and padding to already be computed.
    fn compute_horiz(&self,
                     width: MaybeAuto,
                     left_margin: MaybeAuto,
                     right_margin: MaybeAuto,
                     available_width: Au)
                     -> (Au, Au, Au) {
        // If width is not 'auto', and width + margins > available_width, all 'auto' margins are
        // treated as 0.
        let (left_margin, right_margin) = match width {
            Auto => (left_margin, right_margin),
            Specified(width) => {
                let left = left_margin.specified_or_zero();
                let right = right_margin.specified_or_zero();

                if((left + right + width) > available_width) {
                    (Specified(left), Specified(right))
                } else {
                    (left_margin, right_margin)
                }
            }
        };

        //Invariant: left_margin_Au + width_Au + right_margin_Au == available_width
        let (left_margin_Au, width_Au, right_margin_Au) = match (left_margin, width, right_margin) {
            //If all have a computed value other than 'auto', the system is over-constrained and we need to discard a margin.
            //if direction is ltr, ignore the specified right margin and solve for it. If it is rtl, ignore the specified
            //left margin. FIXME(eatkinson): this assumes the direction is ltr
            (Specified(margin_l), Specified(width), Specified(_margin_r)) => (margin_l, width, available_width - (margin_l + width )),

            //If exactly one value is 'auto', solve for it
            (Auto, Specified(width), Specified(margin_r)) => (available_width - (width + margin_r), width, margin_r),
            (Specified(margin_l), Auto, Specified(margin_r)) => (margin_l, available_width - (margin_l + margin_r), margin_r),
            (Specified(margin_l), Specified(width), Auto) => (margin_l, width, available_width - (margin_l + width)),

            //If width is set to 'auto', any other 'auto' value becomes '0', and width is solved for
            (Auto, Auto, Specified(margin_r)) => (Au::new(0), available_width - margin_r, margin_r),
            (Specified(margin_l), Auto, Auto) => (margin_l, available_width - margin_l, Au::new(0)),
            (Auto, Auto, Auto) => (Au::new(0), available_width, Au::new(0)),

            //If left and right margins are auto, they become equal
            (Auto, Specified(width), Auto) => {
                let margin = (available_width - width).scale_by(0.5);
                (margin, width, margin)
            }

        };
        //return values in same order as params
        (width_Au, left_margin_Au, right_margin_Au)
    }

    fn compute_table_margins(&self, box: &Box, remaining_width: Au, available_width: Au)
                             -> (Au, Au, Au) {
        let style = box.style();

        let (width, maybe_margin_left, maybe_margin_right) =
            (MaybeAuto::from_style(style.Box.width, remaining_width),
             MaybeAuto::from_style(style.Margin.margin_left, remaining_width),
             MaybeAuto::from_style(style.Margin.margin_right, remaining_width));

        let (width, margin_left, margin_right) = self.compute_horiz(width,
                                                                    maybe_margin_left,
                                                                    maybe_margin_right,
                                                                    available_width);

        // If the tentative used width is greater than 'max-width', width should be recalculated,
        // but this time using the computed value of 'max-width' as the computed value for 'width'.
        let (width, margin_left, margin_right) = {
            match specified_or_none(style.Box.max_width, remaining_width) {
                Some(value) if value < width => self.compute_horiz(Specified(value),
                                                                   maybe_margin_left,
                                                                   maybe_margin_right,
                                                                   available_width),
                _ => (width, margin_left, margin_right)
            }
        };

        // If the resulting width is smaller than 'min-width', width should be recalculated,
        // but this time using the value of 'min-width' as the computed value for 'width'.
        let (width, margin_left, margin_right) = {
            let computed_min_width = specified(style.Box.min_width, remaining_width);
            if computed_min_width > width {
                self.compute_horiz(Specified(computed_min_width),
                                   maybe_margin_left,
                                   maybe_margin_right,
                                   available_width)
            } else {
                (width, margin_left, margin_right)
            }
        };

        return (width, margin_left, margin_right);
    }

    fn compute_float_margins(&self, box: &Box, remaining_width: Au) -> (Au, Au, Au) {
        let style = box.style();
        let margin_left = MaybeAuto::from_style(style.Margin.margin_left,
                                                remaining_width).specified_or_zero();
        let margin_right = MaybeAuto::from_style(style.Margin.margin_right,
                                                 remaining_width).specified_or_zero();
        let shrink_to_fit = geometry::min(self.base.pref_width,
                                          geometry::max(self.base.min_width, remaining_width));
        let width = MaybeAuto::from_style(style.Box.width,
                                          remaining_width).specified_or_default(shrink_to_fit);
        debug!("assign_widths_float -- width: {}", width);
        return (width, margin_left, margin_right);
    }

    // inline(always) because this is only ever called by in-order or non-in-order top-level
    // methods
    #[inline(always)]
    fn assign_height_block_base(&mut self, ctx: &mut LayoutContext, inorder: bool) {
        let mut cur_y = Au::new(0);
        let mut clearance = Au::new(0);
        let mut top_offset = Au::new(0);
        let mut bottom_offset = Au::new(0);
        let mut left_offset = Au::new(0);
        let mut float_ctx = Invalid;

        for box in self.box.iter() {
            clearance = match box.clear() {
                None => Au::new(0),
                Some(clear) => {
                    self.base.floats_in.clearance(clear)
                }
            };

            top_offset = clearance + box.margin.get().top + box.border.get().top +
                box.padding.get().top;
            cur_y = cur_y + top_offset;
            bottom_offset = box.margin.get().bottom + box.border.get().bottom +
                box.padding.get().bottom;
            left_offset = box.offset();
        }

        if inorder {
            // Floats for blocks work like this:
            // self.floats_in -> child[0].floats_in
            // visit child[0]
            // child[i-1].floats_out -> child[i].floats_in
            // visit child[i]
            // repeat until all children are visited.
            // last_child.floats_out -> self.floats_out (done at the end of this method)
            float_ctx = self.base.floats_in.translate(Point2D(-left_offset, -top_offset));
            for kid in self.base.child_iter() {
                flow::mut_base(*kid).floats_in = float_ctx.clone();
                kid.assign_height_inorder(ctx);
                float_ctx = flow::mut_base(*kid).floats_out.clone();
            }
        }

        let mut collapsible = Au::new(0);
        let mut collapsing = Au::new(0);
        let mut margin_top = Au::new(0);
        let mut margin_bottom = Au::new(0);
        let mut top_margin_collapsible = false;
        let mut bottom_margin_collapsible = false;
        let mut first_in_flow = true;
        for box in self.box.iter() {
            if box.border.get().top == Au(0) && box.padding.get().top == Au(0) {
                collapsible = box.margin.get().top;
                top_margin_collapsible = true;
            }
            if box.border.get().bottom == Au(0) &&
                    box.padding.get().bottom == Au(0) {
                bottom_margin_collapsible = true;
            }
            margin_top = box.margin.get().top;
            margin_bottom = box.margin.get().bottom;
        }

        for kid in self.base.child_iter() {
            kid.collapse_margins(top_margin_collapsible,
                                 &mut first_in_flow,
                                 &mut margin_top,
                                 &mut top_offset,
                                 &mut collapsing,
                                 &mut collapsible);

            let child_node = flow::mut_base(*kid);
            cur_y = cur_y - collapsing;
            child_node.position.origin.y = cur_y;
            cur_y = cur_y + child_node.position.size.height;
        }

        // The bottom margin collapses with its last in-flow block-level child's bottom margin
        // if the parent has no bottom boder, no bottom padding.
        collapsing = if bottom_margin_collapsible {
            if margin_bottom < collapsible {
                margin_bottom = collapsible;
            }
            collapsible
        } else {
            Au::new(0)
        };

        // TODO: A box's own margins collapse if the 'min-height' property is zero, and it has neither
        // top or bottom borders nor top or bottom padding, and it has a 'height' of either 0 or 'auto',
        // and it does not contain a line box, and all of its in-flow children's margins (if any) collapse.


        let mut height = 
            cur_y - top_offset - collapsing
        ;

        for box in self.box.iter() {
            let style = box.style();

            // At this point, `height` is the height of the containing block, so passing `height`
            // as the second argument here effectively makes percentages relative to the containing
            // block per CSS 2.1 § 10.5.
            height = match MaybeAuto::from_style(style.Box.height, height) {
                Auto => height,
                Specified(value) => value
            };
        }

        let mut noncontent_height = Au::new(0);
        for box in self.box.iter() {
            let mut position = box.position.get();
            let mut margin = box.margin.get();

            // The associated box is the border box of this flow.
            margin.top = margin_top;
            margin.bottom = margin_bottom;

            position.origin.y = clearance + margin.top;

            noncontent_height = box.padding.get().top + box.padding.get().bottom +
                box.border.get().top + box.border.get().bottom;
            position.size.height = height + noncontent_height;

            noncontent_height = noncontent_height + clearance + margin.top + margin.bottom;

            box.position.set(position);
            box.margin.set(margin);
        }

        self.base.position.size.height = height + noncontent_height;

        if inorder {
            let extra_height = height - (cur_y - top_offset) + bottom_offset;
            self.base.floats_out = float_ctx.translate(Point2D(left_offset, -extra_height));
        } else {
            self.base.floats_out = self.base.floats_in.clone();
        }
    }

    fn assign_height_float_inorder(&mut self) {
        // assign_height_float was already called by the traversal function
        // so this is well-defined

        let mut height = Au(0);
        let mut clearance = Au(0);
        let mut full_noncontent_width = Au(0);
        let mut margin_height = Au(0);

        for box in self.box.iter() {
            height = box.position.get().size.height;
            clearance = match box.clear() {
                None => Au(0),
                Some(clear) => self.base.floats_in.clearance(clear),
            };

            let noncontent_width = box.padding.get().left + box.padding.get().right +
                box.border.get().left + box.border.get().right;

            full_noncontent_width = noncontent_width + box.margin.get().left +
                box.margin.get().right;
            margin_height = box.margin.get().top + box.margin.get().bottom;
        }

        let info = PlacementInfo {
            width: self.base.position.size.width + full_noncontent_width,
            height: height + margin_height,
            ceiling: clearance,
            max_width: self.float.get_ref().containing_width,
            f_type: self.float.get_ref().float_type,
        };

        // Place the float and return the FloatContext back to the parent flow.
        // After, grab the position and use that to set our position.
        self.base.floats_out = self.base.floats_in.add_float(&info);
        self.float.get_mut_ref().rel_pos = self.base.floats_out.last_float_pos();
    }

    fn assign_height_float(&mut self, ctx: &mut LayoutContext) {
        // Now that we've determined our height, propagate that out.
        let has_inorder_children = self.base.num_floats > 0;
        if has_inorder_children {
            let mut float_ctx = FloatContext::new(self.float.get_ref().floated_children);
            for kid in self.base.child_iter() {
                flow::mut_base(*kid).floats_in = float_ctx.clone();
                kid.assign_height_inorder(ctx);
                float_ctx = flow::mut_base(*kid).floats_out.clone();
            }
        }
        let mut cur_y = Au(0);
        let mut top_offset = Au(0);

        for box in self.box.iter() {
            top_offset = box.margin.get().top + box.border.get().top + box.padding.get().top;
            cur_y = cur_y + top_offset;
        }

        for kid in self.base.child_iter() {
            let child_base = flow::mut_base(*kid);
            child_base.position.origin.y = cur_y;
            cur_y = cur_y + child_base.position.size.height;
        }

        let mut height = cur_y - top_offset;

        let mut noncontent_height;
        let box = self.box.as_ref().unwrap();
        let mut position = box.position.get();

        // The associated box is the border box of this flow.
        position.origin.y = box.margin.get().top;

        noncontent_height = box.padding.get().top + box.padding.get().bottom +
            box.border.get().top + box.border.get().bottom;

        //TODO(eatkinson): compute heights properly using the 'height' property.
        let height_prop = MaybeAuto::from_style(box.style().Box.height,
                                                Au::new(0)).specified_or_zero();

        height = geometry::max(height, height_prop) + noncontent_height;
        debug!("assign_height_float -- height: {}", height);

        position.size.height = height;
        box.position.set(position);
    }

    pub fn build_display_list_table<E:ExtraDisplayListData>(
                                    &mut self,
                                    builder: &DisplayListBuilder,
                                    dirty: &Rect<Au>,
                                    list: &Cell<DisplayList<E>>)
                                    -> bool {
        if self.is_float() {
            return self.build_display_list_float(builder, dirty, list);
        }

        let abs_rect = Rect(self.base.abs_position, self.base.position.size);
        if !abs_rect.intersects(dirty) {
            return true;
        }

        debug!("build_display_list_block: adding display element");

        // add box that starts block context
        for box in self.box.iter() {
            box.build_display_list(builder, dirty, self.base.abs_position, (&*self) as &Flow, list)
        }

        // TODO: handle any out-of-flow elements
        let this_position = self.base.abs_position;
        for child in self.base.child_iter() {
            let child_base = flow::mut_base(*child);
            child_base.abs_position = this_position + child_base.position.origin;
        }

        false
    }

    pub fn build_display_list_float<E:ExtraDisplayListData>(
                                    &mut self,
                                    builder: &DisplayListBuilder,
                                    dirty: &Rect<Au>,
                                    list: &Cell<DisplayList<E>>)
                                    -> bool {
        let abs_rect = Rect(self.base.abs_position, self.base.position.size);
        if !abs_rect.intersects(dirty) {
            return true
        }

        let offset = self.base.abs_position + self.float.get_ref().rel_pos;
        // add box that starts block context
        for box in self.box.iter() {
            box.build_display_list(builder, dirty, offset, (&*self) as &Flow, list)
        }


        // TODO: handle any out-of-flow elements

        // go deeper into the flow tree
        for child in self.base.child_iter() {
            let child_base = flow::mut_base(*child);
            child_base.abs_position = offset + child_base.position.origin;
        }

        false
    }
}

impl Flow for TableWrapperFlow {
    fn class(&self) -> FlowClass {
        TableWrapperFlowClass
    }

    fn as_table_wrapper<'a>(&'a mut self) -> &'a mut TableWrapperFlow {
        self
    }


    /* Recursively (bottom-up) determine the context's preferred and
    minimum widths.  When called on this context, all child contexts
    have had their min/pref widths set. This function must decide
    min/pref widths based on child context widths and dimensions of
    any boxes it is responsible for flowing.  */

    fn bubble_widths(&mut self, _: &mut LayoutContext) {
        let mut min_width = Au::new(0);
        let mut pref_width = Au::new(0);

        /* find max width from child block contexts */
        for child_ctx in self.base.child_iter() {
            assert!(child_ctx.starts_block_flow() || child_ctx.starts_inline_flow());

            if child_ctx.is_table_colgroup() {
                self.col_widths.push_all(child_ctx.as_table_colgroup().min_widths);
                //self.col_boxes.push_all_move(child_ctx.as_table_colgroup().cols);
            } else if child_ctx.is_table() {
                if self.is_fixed_layout {
                    let mut child_widths = child_ctx.as_table().col_widths.mut_iter();
                    for col_width in self.col_widths.mut_iter() {
                        match child_widths.next() {
                            Some(child_width) => {
                                if *col_width == Au(0) {
                                    *col_width = *child_width;
                                }
                            },
                            None => break
                        }
                    }
                }
                let num_child_cells = child_ctx.as_table().cell_min_widths.len();
                let num_col_cells = self.col_widths.len();
                println!("{:?} column(s) from colgroup, but the child has {:?} column(s)", num_col_cells, num_child_cells);
                let diff = if num_child_cells > num_col_cells { 
                    num_child_cells - num_col_cells 
                } else {
                    0
                };
                for _ in range(0, diff) {
                    self.col_widths.push( Au::new(0) );
                }
            } else {
                let child_base = flow::mut_base(*child_ctx);
                min_width = geometry::max(min_width, child_base.min_width);
                pref_width = geometry::max(pref_width, child_base.pref_width);
            }
        }

        /* if not an anonymous block context, add in block box's widths.
           these widths will not include child elements, just padding etc. */
        for box in self.box.iter() {
            {
                // Can compute border width here since it doesn't depend on anything.
                box.compute_borders(box.style())
            }

            let (this_minimum_width, this_preferred_width) = box.minimum_and_preferred_widths();
            min_width = min_width + this_minimum_width;
            pref_width = pref_width + this_preferred_width;
        }

        self.base.min_width = min_width;
        self.base.pref_width = pref_width;

        println!("TABLE: min_width:{} pref_width:{}", min_width, pref_width);
    }

    /// Recursively (top-down) determines the actual width of child contexts and boxes. When called
    /// on this context, the context has had its width set by the parent context.
    ///
    /// Dual boxes consume some width first, and the remainder is assigned to all child (block)
    /// contexts.
    fn assign_widths(&mut self, ctx: &mut LayoutContext) {
        println!("{:?}", self.col_widths);
        debug!("assign_widths({}): assigning width for flow {}",
               if self.is_float() {
                   "float"
               } else {
                   "block"
               },
               self.base.id);

        // The position was set to the containing block by the flow's parent.
        let mut remaining_width = self.base.position.size.width;
        let mut x_offset = Au::new(0);

        for box in self.box.iter() {
            let style = box.style();

            // The text alignment of a block flow is the text alignment of its box's style.
            self.base.flags.set_text_align(style.Text.text_align);

            // Can compute padding here since we know containing block width.
            box.compute_padding(style, remaining_width);

            // Margins are 0 right now so base.noncontent_width() is just borders + padding.
            let available_width = remaining_width - box.noncontent_width();

            // Top and bottom margins for blocks are 0 if auto.
            let margin_top = MaybeAuto::from_style(style.Margin.margin_top,
                                                   remaining_width).specified_or_zero();
            let margin_bottom = MaybeAuto::from_style(style.Margin.margin_bottom,
                                                      remaining_width).specified_or_zero();

            let (width, margin_left, margin_right) = if self.is_float() {
                self.compute_float_margins(box, remaining_width)
            } else {
                self.compute_table_margins(box, remaining_width, available_width)
            };

            box.margin.set(SideOffsets2D::new(margin_top,
                                              margin_right,
                                              margin_bottom,
                                              margin_left));

            x_offset = box.offset();
            remaining_width = width;

            // The associated box is the border box of this flow.
            let position_ref = box.position.mutate();
            position_ref.ptr.origin.x = box.margin.get().left;
            let padding_and_borders = box.padding.get().left + box.padding.get().right +
                box.border.get().left + box.border.get().right;
            position_ref.ptr.size.width = remaining_width + padding_and_borders;
        }

        let has_inorder_children = if self.is_float() {
            self.base.num_floats > 0
        } else {
            self.base.flags.inorder() || self.base.num_floats > 0
        };

        let mut no_width_cnt = Au(0);
        let mut fix_cell_width = Au(0);
        for col_width in self.col_widths.iter() {
            if *col_width == Au(0) {
                no_width_cnt = no_width_cnt.add(&Au(1));
            } else {
                fix_cell_width = fix_cell_width.add(col_width);
            }
        }
        let default_cell_width = if no_width_cnt > Au(0) {
            (remaining_width - fix_cell_width)/no_width_cnt
        } else {
            Au(0)
        };


        for kid in self.base.child_iter() {
            assert!(kid.starts_block_flow() || kid.starts_inline_flow());

            if kid.is_table() {
                kid.as_table().col_widths = self.col_widths.map(|width| {
                    if *width == Au(0) { 
                        default_cell_width 
                    } else {
                        *width
                    }
                });
            }
            
            let child_base = flow::mut_base(*kid);
            child_base.position.origin.x = x_offset;
            child_base.position.size.width = remaining_width;
            child_base.flags.set_inorder(has_inorder_children);

            if !child_base.flags.inorder() {
                child_base.floats_in = FloatContext::new(0);
            }

            // Per CSS 2.1 § 16.3.1, text decoration propagates to all children in flow.
            //
            // TODO(pcwalton): When we have out-of-flow children, don't unconditionally propagate.
            child_base.flags.propagate_text_decoration_from_parent(self.base.flags);

            child_base.flags.propagate_text_alignment_from_parent(self.base.flags)
        }
    }

    fn assign_height_inorder(&mut self, ctx: &mut LayoutContext) {
        if self.is_float() {
            debug!("assign_height_inorder_float: assigning height for float {}", self.base.id);
            self.assign_height_float_inorder();
        } else {
            debug!("assign_height_inorder: assigning height for block {}", self.base.id);
            self.assign_height_block_base(ctx, true);
        }
    }

    fn assign_height(&mut self, ctx: &mut LayoutContext) {
        if self.is_float() {
            debug!("assign_height_float: assigning height for float {}", self.base.id);
            self.assign_height_float(ctx);
        } else {
            debug!("assign_height: assigning height for block {}", self.base.id);
            // This is the only case in which a block flow can start an inorder
            // subtraversal.
            self.assign_height_block_base(ctx, false);
        }
    }

    fn collapse_margins(&mut self,
                        top_margin_collapsible: bool,
                        first_in_flow: &mut bool,
                        margin_top: &mut Au,
                        top_offset: &mut Au,
                        collapsing: &mut Au,
                        collapsible: &mut Au) {
        if self.is_float() {
            // Margins between a floated box and any other box do not collapse.
            *collapsing = Au::new(0);
            return;
        }

        for box in self.box.iter() {
            // The top margin collapses with its first in-flow block-level child's
            // top margin if the parent has no top border, no top padding.
            if *first_in_flow && top_margin_collapsible {
                // If top-margin of parent is less than top-margin of its first child,
                // the parent box goes down until its top is aligned with the child.
                if *margin_top < box.margin.get().top {
                    // TODO: The position of child floats should be updated and this
                    // would influence clearance as well. See #725
                    let extra_margin = box.margin.get().top - *margin_top;
                    *top_offset = *top_offset + extra_margin;
                    *margin_top = box.margin.get().top;
                }
            }
            // The bottom margin of an in-flow block-level element collapses
            // with the top margin of its next in-flow block-level sibling.
            *collapsing = geometry::min(box.margin.get().top, *collapsible);
            *collapsible = box.margin.get().bottom;
        }

        *first_in_flow = false;
    }

    fn debug_str(&self) -> ~str {
        let txt = if self.is_float() {
            ~"FloatFlow: "
        } else {
            ~"TableWrapperFlow: " 
        };
        txt.append(match self.box {
            Some(ref rb) => rb.debug_str(),
            None => ~"",
        })
    }
}

